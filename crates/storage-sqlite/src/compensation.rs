//! Projection of the optional, privately imported source table. No migrations or writes.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::NaiveDate;
use diesel::{
    prelude::*,
    sql_types::{BigInt, Integer, Nullable, Text},
};
use wealthfolio_core::{
    compensation::{CompensationEvidenceRepositoryTrait, PayrollStatementFact},
    Result,
};

use crate::{
    db::{get_connection, DbPool},
    errors::StorageError,
};

pub struct CompensationEvidenceRepository {
    pool: Arc<DbPool>,
}

impl CompensationEvidenceRepository {
    pub fn new(pool: Arc<DbPool>) -> Self {
        Self { pool }
    }
}

#[derive(QueryableByName)]
struct TableCount {
    #[diesel(sql_type = BigInt)]
    count: i64,
}

#[derive(QueryableByName)]
struct SourceRow {
    #[diesel(sql_type = Text)]
    id: String,
    #[diesel(sql_type = Text)]
    statement_date: String,
    #[diesel(sql_type = Nullable<Integer>)]
    tax_year: Option<i32>,
    #[diesel(sql_type = Text)]
    employer: String,
    #[diesel(sql_type = Text)]
    component_group: String,
    #[diesel(sql_type = Text)]
    component_name: String,
    #[diesel(sql_type = Text)]
    amount_signed: String,
    #[diesel(sql_type = Text)]
    currency: String,
    #[diesel(sql_type = Nullable<Text>)]
    source_basis: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    metadata: Option<String>,
    #[diesel(sql_type = Text)]
    updated_at: String,
}

fn load_as_of(
    conn: &mut SqliteConnection,
    first_year: i32,
    last_year: i32,
    end_date: NaiveDate,
) -> Result<Option<Vec<PayrollStatementFact>>> {
    // A read transaction keeps the existence probe and projection consistent.
    conn.transaction::<_, StorageError, _>(|conn| {
        let exists = diesel::sql_query(
            "SELECT COUNT(*) AS count FROM sqlite_master WHERE type = 'table' AND name = 'payroll_statement_events'",
        ).get_result::<TableCount>(conn)?;
        if exists.count == 0 {
            return Ok(None);
        }
        let rows = diesel::sql_query(
            "SELECT id, statement_date, tax_year, employer, component_group, component_name,
                    amount_signed, currency, source_basis, metadata, updated_at
             FROM payroll_statement_events
             WHERE statement_date <= ? AND
               (tax_year BETWEEN ? AND ? OR
                (tax_year IS NULL AND CAST(substr(statement_date, 1, 4) AS INTEGER) BETWEEN ? AND ?))",
        )
        .bind::<Text, _>(end_date.to_string())
        .bind::<Integer, _>(first_year)
        .bind::<Integer, _>(last_year)
        .bind::<Integer, _>(first_year)
        .bind::<Integer, _>(last_year)
        .load::<SourceRow>(conn)?;
        let facts = rows.into_iter().map(|row| {
            let metadata = match row.metadata {
                Some(raw) => serde_json::from_str(&raw).map_err(|_| {
                    StorageError::SerializationError("Invalid compensation metadata JSON".into())
                })?,
                None => serde_json::Value::Null,
            };
            Ok(PayrollStatementFact {
                id: row.id, statement_date: row.statement_date, tax_year: row.tax_year,
                employer: row.employer, component_group: row.component_group,
                component_name: row.component_name, amount_signed: row.amount_signed,
                currency: row.currency, source_basis: row.source_basis, metadata,
                updated_at: row.updated_at,
            })
        }).collect::<std::result::Result<Vec<_>, StorageError>>()?;
        Ok(Some(facts))
    }).map_err(Into::into)
}

#[async_trait]
impl CompensationEvidenceRepositoryTrait for CompensationEvidenceRepository {
    async fn load_as_of(
        &self,
        first_tax_year: i32,
        last_tax_year: i32,
        end_date: NaiveDate,
    ) -> Result<Option<Vec<PayrollStatementFact>>> {
        let mut conn = get_connection(&self.pool)?;
        load_as_of(&mut conn, first_tax_year, last_tax_year, end_date)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diesel::connection::SimpleConnection;

    fn end() -> NaiveDate {
        NaiveDate::from_ymd_opt(2030, 9, 30).unwrap()
    }

    fn database() -> SqliteConnection {
        let mut conn = SqliteConnection::establish(":memory:").unwrap();
        conn.batch_execute(
            "CREATE TABLE payroll_statement_events (
                id TEXT PRIMARY KEY, statement_date TEXT NOT NULL, tax_year INTEGER,
                employer TEXT NOT NULL, component_group TEXT NOT NULL, component_name TEXT NOT NULL,
                amount_signed TEXT NOT NULL, currency TEXT NOT NULL, source_basis TEXT,
                metadata TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
             );
             INSERT INTO payroll_statement_events VALUES
                ('synthetic', '2030-06-30', 2030, 'Synthetic Employer', 'tax',
                 'Federal withholding', '-12.3400', 'USD', 'Synthetic paystub YTD',
                 '{\"period_basis\":\"YTD snapshot, not payday allocation\"}', 'created', 'updated');
             CREATE TABLE activities (id TEXT PRIMARY KEY, amount TEXT);
             INSERT INTO activities VALUES ('cash-ledger', '987.65');
             CREATE TABLE spending_budget_targets (id TEXT PRIMARY KEY, amount TEXT);
             INSERT INTO spending_budget_targets VALUES ('food-budget', '321.09');",
        ).unwrap();
        conn
    }

    #[derive(QueryableByName)]
    struct Amount {
        #[diesel(sql_type = Text)]
        amount: String,
    }

    #[test]
    fn optional_missing_table_is_explicit_and_is_not_created() {
        let mut conn = SqliteConnection::establish(":memory:").unwrap();
        conn.batch_execute("PRAGMA query_only = ON").unwrap();
        assert!(load_as_of(&mut conn, 2030, 2030, end()).unwrap().is_none());
        let count = diesel::sql_query("SELECT COUNT(*) AS count FROM sqlite_master")
            .get_result::<TableCount>(&mut conn)
            .unwrap();
        assert_eq!(count.count, 0);
    }

    #[test]
    fn source_projection_uses_only_reads_and_preserves_cash_and_food_budget() {
        let mut conn = database();
        conn.batch_execute("PRAGMA query_only = ON").unwrap();
        let changes_before = diesel::sql_query("SELECT total_changes() AS count")
            .get_result::<TableCount>(&mut conn)
            .unwrap()
            .count;
        let rows = load_as_of(&mut conn, 2030, 2030, end()).unwrap().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].statement_date, "2030-06-30");
        assert_eq!(rows[0].amount_signed, "-12.3400");
        assert_eq!(
            rows[0].metadata["period_basis"],
            "YTD snapshot, not payday allocation"
        );
        assert_eq!(
            rows[0].source_basis.as_deref(),
            Some("Synthetic paystub YTD")
        );
        assert_eq!(rows[0].updated_at, "updated");
        let repeat = load_as_of(&mut conn, 2030, 2030, end()).unwrap().unwrap();
        assert_eq!(repeat, rows);
        let changes_after = diesel::sql_query("SELECT total_changes() AS count")
            .get_result::<TableCount>(&mut conn)
            .unwrap()
            .count;
        assert_eq!(changes_after, changes_before);
        let cash = diesel::sql_query("SELECT amount FROM activities")
            .get_result::<Amount>(&mut conn)
            .unwrap();
        let budget = diesel::sql_query("SELECT amount FROM spending_budget_targets")
            .get_result::<Amount>(&mut conn)
            .unwrap();
        assert_eq!(cash.amount, "987.65");
        assert_eq!(budget.amount, "321.09");
    }

    #[test]
    fn selected_tax_years_and_as_of_date_filter_source_without_start_date_cutoff() {
        let mut conn = database();
        conn.batch_execute(
            "INSERT INTO payroll_statement_events
             SELECT 'future', '2030-10-31', tax_year, employer, component_group, component_name,
                    amount_signed, currency, source_basis, metadata, created_at, updated_at
             FROM payroll_statement_events WHERE id = 'synthetic';
             INSERT INTO payroll_statement_events
             SELECT 'prior', '2029-12-31', 2029, employer, component_group, component_name,
                    amount_signed, currency, source_basis, metadata, created_at, updated_at
             FROM payroll_statement_events WHERE id = 'synthetic';",
        )
        .unwrap();
        let rows = load_as_of(&mut conn, 2030, 2030, end()).unwrap().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "synthetic");
        assert_eq!(
            load_as_of(&mut conn, 2029, 2030, end())
                .unwrap()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn present_empty_table_is_distinct_from_missing_table() {
        let mut conn = database();
        conn.batch_execute("DELETE FROM payroll_statement_events")
            .unwrap();
        assert!(load_as_of(&mut conn, 2030, 2030, end())
            .unwrap()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn null_provenance_and_tax_year_are_preserved() {
        let mut conn = database();
        conn.batch_execute("UPDATE payroll_statement_events SET tax_year = NULL, metadata = NULL, source_basis = NULL").unwrap();
        let rows = load_as_of(&mut conn, 2030, 2030, end()).unwrap().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tax_year, None);
        assert!(rows[0].metadata.is_null());
        assert_eq!(rows[0].source_basis, None);
    }

    #[test]
    fn malformed_table_is_a_failure_not_unavailable_or_zero() {
        let mut conn = SqliteConnection::establish(":memory:").unwrap();
        conn.batch_execute("CREATE TABLE payroll_statement_events (id TEXT)")
            .unwrap();
        assert!(load_as_of(&mut conn, 2030, 2030, end()).is_err());
    }

    #[test]
    fn invalid_metadata_fails_without_disclosing_source_content() {
        let mut conn = database();
        conn.batch_execute("UPDATE payroll_statement_events SET metadata = 'private-invalid-json'")
            .unwrap();
        let error = load_as_of(&mut conn, 2030, 2030, end()).unwrap_err();
        assert!(error
            .to_string()
            .contains("Invalid compensation metadata JSON"));
        assert!(!error.to_string().contains("private-invalid-json"));
    }
}
