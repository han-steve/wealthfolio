use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Source {
    rows: Option<Vec<PayrollStatementFact>>,
    calls: AtomicUsize,
}

#[async_trait]
impl CompensationEvidenceRepositoryTrait for Source {
    async fn load_as_of(
        &self,
        _: i32,
        _: i32,
        _: NaiveDate,
    ) -> Result<Option<Vec<PayrollStatementFact>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.rows.clone())
    }
}

fn source(rows: Option<Vec<PayrollStatementFact>>) -> Arc<Source> {
    Arc::new(Source {
        rows,
        calls: AtomicUsize::new(0),
    })
}

fn request(start: &str, end: &str) -> CompensationEvidenceRequest {
    CompensationEvidenceRequest {
        start_date: start.into(),
        end_date: end.into(),
    }
}

fn fact(id: &str, date: &str, basis: &str, name: &str) -> PayrollStatementFact {
    PayrollStatementFact {
        id: id.into(),
        statement_date: date.into(),
        tax_year: Some(2030),
        employer: "Synthetic Employer".into(),
        component_group: "gross_income".into(),
        component_name: name.into(),
        amount_signed: "1234.56".into(),
        currency: "USD".into(),
        source_basis: Some(format!("Synthetic {basis} statement")),
        metadata: json!({"period_basis": basis}),
        updated_at: "2031-01-01T00:00:00Z".into(),
    }
}

async fn evidence(rows: Vec<PayrollStatementFact>, start: &str, end: &str) -> CompensationEvidence {
    CompensationEvidenceService::new(source(Some(rows)))
        .get(request(start, end))
        .await
        .unwrap()
}

#[tokio::test]
async fn invalid_dates_are_rejected_before_source_read() {
    let repository = source(None);
    let service = CompensationEvidenceService::new(repository.clone());
    for (start, end) in [
        ("2030-09-30", "2030-07-01"),
        ("2030-02-29", "2030-09-30"),
        ("2030-7-01", "2030-09-30"),
        ("0000-01-01", "2030-09-30"),
        ("2030-01-01T00:00:00Z", "2030-09-30"),
        ("", "2030-09-30"),
    ] {
        assert!(matches!(
            service.get(request(start, end)).await,
            Err(Error::Validation(_))
        ));
    }
    assert_eq!(repository.calls.load(Ordering::SeqCst), 0);
    assert!(request("2032-02-29", "2032-02-29").validate().is_ok());
}

#[tokio::test]
async fn missing_table_is_not_an_empty_income_total() {
    let response = CompensationEvidenceService::new(source(None))
        .get(request("2030-07-01", "2030-09-30"))
        .await
        .unwrap();
    let json = serde_json::to_value(response).unwrap();
    assert_eq!(json["status"], "unavailable");
    assert!(json["unavailableReason"]
        .as_str()
        .unwrap()
        .contains("table is absent"));
    assert_eq!(json["documents"], json!([]));
    assert!(json.get("grossIncome").is_none());
}

#[tokio::test]
async fn present_empty_table_is_available() {
    let response = evidence(vec![], "2030-01-01", "2030-12-31").await;
    assert!(matches!(
        response.status,
        CompensationEvidenceStatus::Available
    ));
    assert!(response.unavailable_reason.is_none());
    assert!(response.documents.is_empty());
}

#[tokio::test]
async fn q3_returns_latest_ytd_without_summing_or_claiming_q3_gross() {
    let rows = vec![
        fact("old", "2030-06-30", "YTD", "Salary"),
        fact("new", "2030-09-30", "YTD", "Salary"),
        fact("future", "2030-10-31", "YTD", "Salary"),
        fact("covered-vest", "2030-08-15", "event", "RSU income"),
    ];
    let response = evidence(rows, "2030-07-01", "2030-09-30").await;
    assert_eq!(response.documents.len(), 1);
    let doc = &response.documents[0];
    assert_eq!(doc.basis, CompensationBasis::Ytd);
    assert_eq!(doc.components.len(), 1);
    assert_eq!(doc.components[0].id, "new");
    assert_eq!(doc.components[0].amount_signed, "1234.56");
    assert_eq!(doc.coverage.start_date.as_deref(), Some("2030-01-01"));
    assert_eq!(doc.coverage.end_date.as_deref(), Some("2030-09-30"));
    assert!(!doc.coverage.matches_selected_period);
    assert!(doc
        .basis_label
        .starts_with("Year-to-date through 2030-09-30"));
}

#[tokio::test]
async fn as_of_includes_pre_window_document_and_post_snapshot_events_separately() {
    let response = evidence(
        vec![
            fact("snapshot", "2030-06-30", "YTD", "Salary"),
            fact("vest", "2030-08-15", "event", "RSU income"),
        ],
        "2030-07-01",
        "2030-09-30",
    )
    .await;
    assert_eq!(response.documents.len(), 2);
    assert_eq!(response.documents[0].components[0].id, "snapshot");
    assert_eq!(response.documents[1].basis, CompensationBasis::Event);
    assert!(!response.documents[0].coverage.matches_selected_period);
}

#[tokio::test]
async fn annual_supersedes_ytd_and_excludes_only_matching_event_components() {
    let response = evidence(
        vec![
            fact("ytd", "2030-09-30", "YTD", "Regular pay"),
            fact("annual", "2030-12-31", "Annual W-2", "Salary"),
            fact("annual-rsu", "2030-12-31", "Annual W-2", "RSU income"),
            fact("vest", "2030-08-15", "event", "RSU income"),
            fact(
                "other",
                "2030-08-15",
                "event",
                "Distinct documented earnings",
            ),
        ],
        "2030-07-01",
        "2030-12-31",
    )
    .await;
    assert_eq!(response.documents.len(), 2);
    assert_eq!(response.documents[0].components[0].id, "other");
    assert_eq!(response.documents[1].components.len(), 2);
    assert_eq!(response.documents[1].basis, CompensationBasis::Annual);
    assert!(!response.documents[1].coverage.matches_selected_period);
    assert!(response
        .documents
        .iter()
        .flat_map(|doc| &doc.components)
        .all(|component| component.id != "ytd" && component.id != "vest"));
}

#[tokio::test]
async fn explicitly_split_annual_salary_preserves_non_overlapping_vest_facts() {
    let mut salary = fact(
        "salary",
        "2030-12-31",
        "annual",
        "Salary / wages excluding RSU",
    );
    salary.metadata = json!({"w2_wages": "2000", "rsu_evidence_total": "1000"});
    salary.source_basis = Some("Synthetic tax return W-2 wages split by vest FMV".into());
    let mut vest = fact("vest", "2030-08-15", "event", "RSU income");
    vest.source_basis = salary.source_basis.clone();
    vest.metadata = json!({"equity_awards_gross_fmv": "1000"});
    let response = evidence(vec![salary, vest], "2030-01-01", "2030-12-31").await;
    assert_eq!(response.documents.len(), 2);
    assert_eq!(response.documents[0].basis, CompensationBasis::Event);
    assert_eq!(response.documents[1].basis, CompensationBasis::Annual);
    assert!(response.documents[1].coverage.matches_selected_period);
}

#[tokio::test]
async fn employers_years_and_currencies_are_never_combined() {
    let a = fact("usd", "2030-06-30", "YTD", "Salary");
    let mut b = a.clone();
    b.id = "eur".into();
    b.currency = "EUR".into();
    let mut c = a.clone();
    c.id = "other".into();
    c.employer = "Another Synthetic Employer".into();
    let mut prior = a.clone();
    prior.id = "prior".into();
    prior.tax_year = Some(2029);
    prior.statement_date = "2029-12-31".into();
    let response = evidence(vec![a, b, c, prior], "2029-07-01", "2030-09-30").await;
    assert_eq!(response.tax_years, vec![2029, 2030]);
    assert_eq!(response.documents.len(), 4);
}

#[tokio::test]
async fn latest_group_snapshot_does_not_keep_obsolete_components() {
    let response = evidence(
        vec![
            fact("old-stipend", "2030-06-30", "YTD", "Stipend"),
            fact("new-salary", "2030-09-30", "YTD", "Salary"),
        ],
        "2030-01-01",
        "2030-09-30",
    )
    .await;
    assert_eq!(response.documents.len(), 1);
    assert_eq!(response.documents[0].components.len(), 1);
    assert_eq!(response.documents[0].components[0].id, "new-salary");
}

#[tokio::test]
async fn documented_sign_precision_provenance_and_groups_are_preserved() {
    let salary = fact("salary", "2030-09-30", "YTD", "Regular pay");
    let mut tax = salary.clone();
    tax.id = "tax".into();
    tax.component_group = "tax".into();
    tax.component_name = "Federal withholding".into();
    tax.amount_signed = "-12.3400".into();
    let mut deduction = tax.clone();
    deduction.id = "deduction".into();
    deduction.component_group = "pre_tax_deduction".into();
    deduction.component_name = "401(k)".into();
    let response = evidence(vec![salary, tax, deduction], "2030-01-01", "2030-09-30").await;
    let json = serde_json::to_value(response).unwrap();
    assert_eq!(json["documents"].as_array().unwrap().len(), 1);
    let components = json["documents"][0]["components"].as_array().unwrap();
    assert_eq!(components.len(), 3);
    let tax = components.iter().find(|row| row["id"] == "tax").unwrap();
    assert_eq!(tax["amountSigned"], "-12.3400");
    assert_eq!(tax["metadata"], json!({"period_basis": "YTD"}));
    assert_eq!(tax["sourceBasis"], "Synthetic YTD statement");
}

#[tokio::test]
async fn unknown_basis_and_missing_tax_year_do_not_manufacture_coverage() {
    let mut unknown = fact("unknown", "2030-09-30", "unspecified", "Salary");
    unknown.tax_year = None;
    unknown.metadata = Value::Null;
    unknown.source_basis = None;
    let response = evidence(vec![unknown], "2030-07-01", "2030-09-30").await;
    let doc = &response.documents[0];
    assert_eq!(doc.basis, CompensationBasis::Unknown);
    assert_eq!(doc.tax_year, None);
    assert_eq!(doc.coverage.start_date, None);
    assert_eq!(doc.coverage.end_date, None);
    assert!(!doc.coverage.matches_selected_period);
}

#[tokio::test]
async fn bad_source_dates_or_amounts_fail_without_disclosing_values() {
    for (date, amount) in [
        ("2030-02-30", "1234"),
        ("2030-09-30", "private-invalid-value"),
    ] {
        let mut row = fact("bad", date, "YTD", "Salary");
        row.amount_signed = amount.into();
        let error = CompensationEvidenceService::new(source(Some(vec![row])))
            .get(request("2030-01-01", "2030-12-31"))
            .await
            .unwrap_err();
        assert!(!error.to_string().contains(amount));
    }
}

#[tokio::test]
async fn duplicate_snapshot_components_choose_latest_revision_not_sum() {
    let old = fact("old", "2030-09-30", "YTD", "Salary");
    let mut new = old.clone();
    new.id = "new".into();
    new.updated_at = "2031-02-01T00:00:00Z".into();
    new.amount_signed = "99".into();
    let response = evidence(vec![new, old], "2030-01-01", "2030-09-30").await;
    assert_eq!(response.documents[0].components.len(), 1);
    assert_eq!(response.documents[0].components[0].id, "new");
    assert_eq!(response.documents[0].components[0].amount_signed, "99");
}

#[test]
fn legacy_prose_basis_is_used_when_period_metadata_is_absent() {
    let mut row = fact("legacy", "2030-09-30", "unused", "Salary");
    row.metadata = json!({"gross_pay_ytd": "1234.56"});
    row.source_basis = Some("Synthetic paystub YTD employee taxes".into());
    assert_eq!(classify(&row), CompensationBasis::Ytd);
    row.source_basis = Some("Synthetic annual tax return W-2 withholding".into());
    assert_eq!(classify(&row), CompensationBasis::Annual);
    row.metadata = json!({"equity_awards_gross_fmv": "1234.56"});
    assert_eq!(classify(&row), CompensationBasis::Event);
}

#[tokio::test]
async fn deduction_source_paystub_ytd_is_cumulative_without_period_metadata_and_source_is_immutable(
) {
    let mut deduction = fact("deduction", "2030-09-30", "unused", "Medical insurance");
    deduction.component_group = "pre_tax_deduction".into();
    deduction.source_basis = Some("Synthetic paystubYTD employee pre-tax deductions".into());
    deduction.metadata = json!({"pre_tax_deductions_ytd": "12.34"});
    deduction.amount_signed = "-12.34".into();
    let original = vec![deduction];
    let repository = source(Some(original.clone()));
    let response = CompensationEvidenceService::new(repository.clone())
        .get(request("2030-07-01", "2030-09-30"))
        .await
        .unwrap();
    assert_eq!(response.documents[0].basis, CompensationBasis::Ytd);
    assert!(!response.documents[0].coverage.matches_selected_period);
    assert_eq!(response.documents[0].components[0].amount_signed, "-12.34");
    assert_eq!(repository.rows.as_ref().unwrap(), &original);
}

#[tokio::test]
async fn tax_year_not_document_year_selects_a_prior_year_annual_filing() {
    let mut annual = fact("filing", "2031-03-15", "annual", "W-2 wages");
    annual.tax_year = Some(2030);
    let response = evidence(vec![annual], "2030-07-01", "2031-09-30").await;
    assert_eq!(response.documents.len(), 1);
    let doc = &response.documents[0];
    assert_eq!(doc.tax_year, Some(2030));
    assert_eq!(doc.coverage.start_date.as_deref(), Some("2030-01-01"));
    assert_eq!(doc.coverage.end_date.as_deref(), Some("2030-12-31"));
    assert!(!doc.coverage.matches_selected_period);
}

#[tokio::test]
async fn year_to_date_snapshots_in_all_time_view_stay_separate_per_year() {
    let mut prior = fact("prior", "2029-09-30", "YTD", "Salary");
    prior.tax_year = Some(2029);
    let current = fact("current", "2030-09-30", "YTD", "Salary");
    let response = evidence(vec![prior, current], "2029-01-01", "2030-09-30").await;
    assert_eq!(response.documents.len(), 2);
    assert_eq!(response.documents[0].tax_year, Some(2029));
    assert_eq!(response.documents[1].tax_year, Some(2030));
    assert!(response
        .documents
        .iter()
        .all(|doc| !doc.coverage.matches_selected_period));
}
