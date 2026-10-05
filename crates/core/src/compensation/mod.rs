//! Read-only documented compensation, independent of cash activities and budgets.

use std::{collections::BTreeMap, str::FromStr, sync::Arc};

use async_trait::async_trait;
use chrono::{Datelike, NaiveDate};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{Error, Result, ValidationError};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompensationEvidenceRequest {
    pub start_date: String,
    pub end_date: String,
}

impl CompensationEvidenceRequest {
    pub fn validate(&self) -> Result<(NaiveDate, NaiveDate)> {
        let start = parse_date(&self.start_date).ok_or_else(invalid_range)?;
        let end = parse_date(&self.end_date).ok_or_else(invalid_range)?;
        if start > end {
            return Err(invalid_range());
        }
        Ok((start, end))
    }
}

fn invalid_range() -> Error {
    ValidationError::InvalidInput(
        "Compensation dates must be YYYY-MM-DD and startDate must be on or before endDate".into(),
    )
    .into()
}

fn parse_date(value: &str) -> Option<NaiveDate> {
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
    (value.len() == 10 && date.year() >= 1 && date.to_string() == value).then_some(date)
}

/// An existing source row, never an activity or an inferred bank deposit.
#[derive(Debug, Clone, PartialEq)]
pub struct PayrollStatementFact {
    pub id: String,
    pub statement_date: String,
    pub tax_year: Option<i32>,
    pub employer: String,
    pub component_group: String,
    pub component_name: String,
    pub amount_signed: String,
    pub currency: String,
    pub source_basis: Option<String>,
    pub metadata: Value,
    pub updated_at: String,
}

#[async_trait]
pub trait CompensationEvidenceRepositoryTrait: Send + Sync {
    /// None means the optional source table is absent; Some([]) means no facts.
    async fn load_as_of(
        &self,
        first_tax_year: i32,
        last_tax_year: i32,
        end_date: NaiveDate,
    ) -> Result<Option<Vec<PayrollStatementFact>>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CompensationBasis {
    Annual,
    Ytd,
    Event,
    Unknown,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CompensationEvidenceStatus {
    Available,
    Unavailable,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompensationCoverage {
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub matches_selected_period: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompensationComponent {
    pub id: String,
    pub component_group: String,
    pub component_name: String,
    pub amount_signed: String,
    pub source_basis: Option<String>,
    pub metadata: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompensationDocument {
    pub statement_date: String,
    pub tax_year: Option<i32>,
    pub employer: String,
    pub currency: String,
    pub basis: CompensationBasis,
    pub basis_label: String,
    pub coverage: CompensationCoverage,
    pub components: Vec<CompensationComponent>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompensationEvidence {
    pub status: CompensationEvidenceStatus,
    pub unavailable_reason: Option<String>,
    pub start_date: String,
    pub end_date: String,
    pub tax_years: Vec<i32>,
    pub selection_policy: String,
    pub documents: Vec<CompensationDocument>,
}

const SELECTION_POLICY: &str = "Selected tax years, documents on or before endDate (including before startDate). Latest annual or YTD document per employer/tax year/currency/component group; annual supersedes YTD. Covered YTD events and matching annual components are excluded. Distinct annual salary and RSU events remain separate. Documents and overlapping wage bases are not additive; no selected-period gross income or cash-deposit total is calculated.";

pub struct CompensationEvidenceService {
    repository: Arc<dyn CompensationEvidenceRepositoryTrait>,
}

impl CompensationEvidenceService {
    pub fn new(repository: Arc<dyn CompensationEvidenceRepositoryTrait>) -> Self {
        Self { repository }
    }

    pub async fn get(&self, request: CompensationEvidenceRequest) -> Result<CompensationEvidence> {
        let (start, end) = request.validate()?;
        let rows = self
            .repository
            .load_as_of(start.year(), end.year(), end)
            .await?;
        let mut response = CompensationEvidence {
            status: CompensationEvidenceStatus::Available,
            unavailable_reason: None,
            start_date: request.start_date,
            end_date: request.end_date,
            tax_years: (start.year()..=end.year()).collect(),
            selection_policy: SELECTION_POLICY.into(),
            documents: Vec::new(),
        };
        match rows {
            None => {
                response.status = CompensationEvidenceStatus::Unavailable;
                response.unavailable_reason =
                    Some("payroll_statement_events table is absent".into());
            }
            Some(rows) => response.documents = select_documents(rows, start, end)?,
        }
        Ok(response)
    }
}

/// Legacy importer facts have prose provenance rather than a normalized basis.
/// Prefer explicit period metadata, then the importer's documented source markers.
fn classify(row: &PayrollStatementFact) -> CompensationBasis {
    let period = row.metadata.get("period_basis").and_then(Value::as_str);
    let explicit = row.metadata.get("basis").and_then(Value::as_str);
    let marker = period.or(explicit).unwrap_or("").to_ascii_lowercase();
    let source = row
        .source_basis
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    if marker.contains("ytd") || marker.contains("year-to-date") {
        return CompensationBasis::Ytd;
    }
    if marker.contains("annual") {
        return CompensationBasis::Annual;
    }
    if marker == "event" {
        return CompensationBasis::Event;
    }
    // Vest facts may cite annual W-2 reconciliation, but are still dated events.
    if row.metadata.get("equity_awards_gross_fmv").is_some() {
        return CompensationBasis::Event;
    }
    if source.contains("ytd") || source.contains("year-to-date") {
        CompensationBasis::Ytd
    } else if source.contains("w-2")
        || source.contains("w2")
        || source.contains("tax return")
        || source.contains("annual")
    {
        CompensationBasis::Annual
    } else if source.contains("vest") {
        CompensationBasis::Event
    } else {
        CompensationBasis::Unknown
    }
}

type GroupKey = (String, i32, String, String);

fn group_key(row: &PayrollStatementFact) -> GroupKey {
    (
        row.employer.clone(),
        row.tax_year.unwrap_or_else(|| {
            parse_date(&row.statement_date)
                .expect("validated statement date")
                .year()
        }),
        row.currency.clone(),
        row.component_group.clone(),
    )
}

fn coverage(
    row: &PayrollStatementFact,
    basis: CompensationBasis,
) -> (Option<String>, Option<String>) {
    match basis {
        CompensationBasis::Annual | CompensationBasis::Ytd => {
            let Some(year) = row.tax_year else {
                return (None, None);
            };
            let start = Some(format!("{year:04}-01-01"));
            let end = if basis == CompensationBasis::Annual {
                Some(format!("{year:04}-12-31"))
            } else if row.statement_date.starts_with(&format!("{year:04}-")) {
                Some(row.statement_date.clone())
            } else {
                // A document from another year does not establish a YTD cutoff.
                return (None, None);
            };
            (start, end)
        }
        CompensationBasis::Event => (
            Some(row.statement_date.clone()),
            Some(row.statement_date.clone()),
        ),
        CompensationBasis::Unknown => (None, None),
    }
}

fn select_documents(
    rows: Vec<PayrollStatementFact>,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Vec<CompensationDocument>> {
    let mut facts = Vec::new();
    for row in rows {
        let date = parse_date(&row.statement_date)
            .ok_or_else(|| Error::Repository("Invalid compensation statement date".into()))?;
        if row.tax_year.is_some_and(|year| !(1..=9999).contains(&year))
            || Decimal::from_str(&row.amount_signed).is_err()
        {
            return Err(Error::Repository("Invalid compensation source fact".into()));
        }
        let year = row.tax_year.unwrap_or(date.year());
        if date <= end && (start.year()..=end.year()).contains(&year) {
            let basis = classify(&row);
            facts.push((row, basis));
        }
    }

    // Select whole component-group snapshots, not a mix of stale component rows.
    let mut snapshots: BTreeMap<GroupKey, (CompensationBasis, String)> = BTreeMap::new();
    for (row, basis) in &facts {
        if matches!(basis, CompensationBasis::Annual | CompensationBasis::Ytd) {
            let candidate = (*basis, row.statement_date.clone());
            let current = snapshots.entry(group_key(row)).or_insert(candidate.clone());
            if (candidate.0 == CompensationBasis::Annual && current.0 == CompensationBasis::Ytd)
                || (candidate.0 == current.0 && candidate.1 > current.1)
            {
                *current = candidate;
            }
        }
    }

    facts.retain(|(row, basis)| {
        let Some((selected_basis, date)) = snapshots.get(&group_key(row)) else {
            return true;
        };
        match basis {
            CompensationBasis::Annual | CompensationBasis::Ytd => {
                basis == selected_basis && &row.statement_date == date
            }
            CompensationBasis::Event if *selected_basis == CompensationBasis::Ytd => {
                // A cumulative group already covers events up through its cutoff.
                &row.statement_date > date
            }
            _ => true,
        }
    });

    let annual_components: Vec<_> = facts
        .iter()
        .filter(|(_, basis)| *basis == CompensationBasis::Annual)
        .map(|(row, _)| (group_key(row), row.component_name.clone()))
        .collect();
    facts.retain(|(row, basis)| {
        *basis != CompensationBasis::Event
            || !annual_components.contains(&(group_key(row), row.component_name.clone()))
    });

    // Deterministic ordering, retaining the latest revision of a snapshot component.
    facts.sort_by(|(a, ab), (b, bb)| {
        (
            &a.statement_date,
            &a.employer,
            a.tax_year,
            &a.currency,
            ab,
            &a.component_group,
            &a.component_name,
            &a.updated_at,
            &a.id,
        )
            .cmp(&(
                &b.statement_date,
                &b.employer,
                b.tax_year,
                &b.currency,
                bb,
                &b.component_group,
                &b.component_name,
                &b.updated_at,
                &b.id,
            ))
    });
    let mut documents: Vec<CompensationDocument> = Vec::new();
    for (row, basis) in facts {
        let same_document = documents.last().is_some_and(|doc| {
            doc.statement_date == row.statement_date
                && doc.tax_year == row.tax_year
                && doc.employer == row.employer
                && doc.currency == row.currency
                && doc.basis == basis
        });
        if !same_document {
            let (coverage_start, coverage_end) = coverage(&row, basis);
            let matches_selected_period = coverage_start.as_deref() == Some(&start.to_string())
                && coverage_end.as_deref() == Some(&end.to_string());
            let label = match basis {
                CompensationBasis::Annual => {
                    format!("Annual as of document date {}", row.statement_date)
                }
                CompensationBasis::Ytd => format!(
                    "Year-to-date through {} (YTD as of document date)",
                    row.statement_date
                ),
                CompensationBasis::Event => format!("Documented event on {}", row.statement_date),
                CompensationBasis::Unknown => "Period coverage not documented".into(),
            };
            documents.push(CompensationDocument {
                statement_date: row.statement_date.clone(),
                tax_year: row.tax_year,
                employer: row.employer.clone(),
                currency: row.currency.clone(),
                basis,
                basis_label: label,
                coverage: CompensationCoverage {
                    start_date: coverage_start,
                    end_date: coverage_end,
                    matches_selected_period,
                },
                components: Vec::new(),
            });
        }
        let doc = documents.last_mut().expect("document just inserted");
        let component = CompensationComponent {
            id: row.id,
            component_group: row.component_group,
            component_name: row.component_name,
            amount_signed: row.amount_signed,
            source_basis: row.source_basis,
            metadata: row.metadata,
        };
        if matches!(basis, CompensationBasis::Annual | CompensationBasis::Ytd)
            && doc.components.last().is_some_and(|previous| {
                previous.component_group == component.component_group
                    && previous.component_name == component.component_name
            })
        {
            *doc.components.last_mut().unwrap() = component;
        } else {
            doc.components.push(component);
        }
    }
    Ok(documents)
}

#[cfg(test)]
mod tests;
