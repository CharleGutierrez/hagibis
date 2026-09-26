use hgb_core::db_sentinel::{
    ColumnSchema, MigrationSafetyLevel, SchemaDriftItem, SchemaDriftReport, TableSchema,
};
use hgb_core::{HgbError, Result};
use rusqlite::Connection;
use std::path::Path;

pub struct DbSentinel;

impl DbSentinel {
    /// Introspect a live SQLite database and return its table schemas
    pub fn introspect_sqlite<P: AsRef<Path>>(db_path: P) -> Result<Vec<TableSchema>> {
        let conn = Connection::open(db_path.as_ref())
            .map_err(|e| HgbError::Storage(format!("Failed to open SQLite database: {}", e)))?;

        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .map_err(|e| HgbError::Storage(format!("Failed to query sqlite_master: {}", e)))?;

        let table_names: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| HgbError::Storage(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        let mut tables = Vec::new();
        for table_name in table_names {
            let mut pragma_stmt = conn
                .prepare(&format!("PRAGMA table_info(\"{}\")", table_name))
                .map_err(|e| HgbError::Storage(format!("Failed to prepare PRAGMA table_info: {}", e)))?;

            let mut columns = Vec::new();
            let mut primary_keys = Vec::new();

            let col_rows = pragma_stmt
                .query_map([], |row| {
                    let name: String = row.get(1)?;
                    let data_type: String = row.get(2)?;
                    let notnull: i32 = row.get(3)?;
                    let default_val: Option<String> = row.get(4)?;
                    let pk: i32 = row.get(5)?;
                    Ok((name, data_type, notnull == 0 && pk == 0, pk > 0, default_val))
                })
                .map_err(|e| HgbError::Storage(e.to_string()))?;

            for r in col_rows {
                if let Ok((name, data_type, is_nullable, is_primary_key, default_value)) = r {
                    if is_primary_key {
                        primary_keys.push(name.clone());
                    }
                    columns.push(ColumnSchema {
                        name,
                        data_type: if data_type.is_empty() { "TEXT".to_string() } else { data_type.to_uppercase() },
                        is_nullable,
                        is_primary_key,
                        default_value,
                    });
                }
            }

            tables.push(TableSchema {
                table_name,
                columns,
                primary_keys,
                foreign_keys: Vec::new(),
            });
        }

        Ok(tables)
    }

    /// Parse simple SQL DDL statements (e.g. schema.sql) into TableSchemas
    pub fn parse_sql_ddl(ddl_content: &str) -> Result<Vec<TableSchema>> {
        let mut tables = Vec::new();
        let re_table = regex::Regex::new(r"(?is)CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?(?:[\x22`']?(\w+)[\x22`']?\.)?[\x22`']?(\w+)[\x22`']?\s*\((.*?)\);")
            .map_err(|e| HgbError::Storage(e.to_string()))?;

        for cap in re_table.captures_iter(ddl_content) {
            let table_name = cap.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
            let body = cap.get(3).map(|m| m.as_str()).unwrap_or_default();

            let mut columns = Vec::new();
            let mut primary_keys = Vec::new();

            for line in body.split(',') {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let upper = trimmed.to_uppercase();
                if upper.starts_with("PRIMARY KEY") || upper.starts_with("FOREIGN KEY") || upper.starts_with("CONSTRAINT") || upper.starts_with("UNIQUE") {
                    if upper.starts_with("PRIMARY KEY") {
                        if let Some(start) = trimmed.find('(') {
                            if let Some(end) = trimmed.rfind(')') {
                                let pk_col = trimmed[start + 1..end].trim().replace(['"', '`', '\''], "");
                                primary_keys.push(pk_col);
                            }
                        }
                    }
                    continue;
                }

                let tokens: Vec<&str> = trimmed.split_whitespace().collect();
                if tokens.len() >= 2 {
                    let col_name = tokens[0].replace(['"', '`', '\''], "");
                    let data_type = tokens[1].to_uppercase();
                    let not_null = upper.contains("NOT NULL");
                    let is_pk = upper.contains("PRIMARY KEY");
                    if is_pk {
                        primary_keys.push(col_name.clone());
                    }

                    let default_val = if let Some(idx) = upper.find("DEFAULT ") {
                        let rem = &trimmed[idx + 8..];
                        let val = rem.split_whitespace().next().unwrap_or("").trim_matches([';', ',', '\'']).to_string();
                        Some(val)
                    } else {
                        None
                    };

                    columns.push(ColumnSchema {
                        name: col_name,
                        data_type,
                        is_nullable: !not_null && !is_pk,
                        is_primary_key: is_pk,
                        default_value: default_val,
                    });
                }
            }

            tables.push(TableSchema {
                table_name,
                columns,
                primary_keys,
                foreign_keys: Vec::new(),
            });
        }

        Ok(tables)
    }

    /// Detect schema drift between live SQLite database and expected DDL
    pub fn detect_drift<P: AsRef<Path>>(db_path: P, ddl_content: &str) -> Result<SchemaDriftReport> {
        let live_tables = Self::introspect_sqlite(db_path.as_ref())?;
        let expected_tables = Self::parse_sql_ddl(ddl_content)?;

        let mut drift_items = Vec::new();
        let mut forward_sqls = Vec::new();
        let mut rollback_sqls = Vec::new();
        let mut overall_safety = MigrationSafetyLevel::SafeAdditive;

        for expected in &expected_tables {
            if let Some(live) = live_tables.iter().find(|t| t.table_name.eq_ignore_ascii_case(&expected.table_name)) {
                // Check columns
                for exp_col in &expected.columns {
                    if let Some(live_col) = live.columns.iter().find(|c| c.name.eq_ignore_ascii_case(&exp_col.name)) {
                        // Check type compatibility
                        if !live_col.data_type.eq_ignore_ascii_case(&exp_col.data_type) {
                            drift_items.push(SchemaDriftItem::ColumnTypeMismatch {
                                table_name: expected.table_name.clone(),
                                column_name: exp_col.name.clone(),
                                expected_type: exp_col.data_type.clone(),
                                live_type: live_col.data_type.clone(),
                            });
                            overall_safety = MigrationSafetyLevel::DestructiveDrop;
                        }
                        if live_col.is_nullable != exp_col.is_nullable {
                            drift_items.push(SchemaDriftItem::NullabilityMismatch {
                                table_name: expected.table_name.clone(),
                                column_name: exp_col.name.clone(),
                                expected_nullable: exp_col.is_nullable,
                                live_nullable: live_col.is_nullable,
                            });
                        }
                    } else {
                        // Missing column in live
                        drift_items.push(SchemaDriftItem::MissingColumn {
                            table_name: expected.table_name.clone(),
                            column: exp_col.clone(),
                        });

                        let default_clause = match &exp_col.default_value {
                            Some(d) => format!(" DEFAULT '{}'", d),
                            None => String::new(),
                        };
                        let null_clause = if exp_col.is_nullable { "" } else { " NOT NULL" };

                        if !exp_col.is_nullable && exp_col.default_value.is_none() {
                            if overall_safety != MigrationSafetyLevel::DestructiveDrop {
                                overall_safety = MigrationSafetyLevel::RequiresBackfill;
                            }
                        }

                        forward_sqls.push(format!(
                            "ALTER TABLE \"{}\" ADD COLUMN \"{}\" {}{}{};",
                            expected.table_name, exp_col.name, exp_col.data_type, null_clause, default_clause
                        ));
                        rollback_sqls.push(format!(
                            "-- SQLite 3.35+ supports DROP COLUMN; earlier requires table recreate\nALTER TABLE \"{}\" DROP COLUMN \"{}\";",
                            expected.table_name, exp_col.name
                        ));
                    }
                }
            } else {
                // Entire table missing in live
                drift_items.push(SchemaDriftItem::MissingTable {
                    table_name: expected.table_name.clone(),
                });

                let mut col_defs = Vec::new();
                for c in &expected.columns {
                    let null_clause = if c.is_nullable { "" } else { " NOT NULL" };
                    let pk_clause = if c.is_primary_key { " PRIMARY KEY" } else { "" };
                    let dflt_clause = match &c.default_value {
                        Some(d) => format!(" DEFAULT '{}'", d),
                        None => String::new(),
                    };
                    col_defs.push(format!("\"{}\" {}{}{}{}", c.name, c.data_type, pk_clause, null_clause, dflt_clause));
                }

                forward_sqls.push(format!(
                    "CREATE TABLE IF NOT EXISTS \"{}\" (\n  {}\n);",
                    expected.table_name,
                    col_defs.join(",\n  ")
                ));
                rollback_sqls.push(format!("DROP TABLE IF EXISTS \"{}\";", expected.table_name));
            }
        }

        // Check for extra tables in live
        for live in &live_tables {
            if !expected_tables.iter().any(|t| t.table_name.eq_ignore_ascii_case(&live.table_name)) {
                drift_items.push(SchemaDriftItem::ExtraTable {
                    table_name: live.table_name.clone(),
                });
            }
        }

        let is_in_sync = drift_items.is_empty();

        Ok(SchemaDriftReport {
            database_path: db_path.as_ref().display().to_string(),
            is_in_sync,
            drift_items,
            overall_safety,
            generated_forward_sql: forward_sqls.join("\n\n"),
            generated_rollback_sql: rollback_sqls.join("\n\n"),
        })
    }

    /// Perform a safe, transactional dry-run using a savepoint to verify migration DDL
    pub fn dry_run_migration<P: AsRef<Path>>(db_path: P, migration_sql: &str) -> Result<()> {
        let mut conn = Connection::open(db_path.as_ref())
            .map_err(|e| HgbError::Storage(format!("Failed to open DB for dry run: {}", e)))?;

        let mut sp = conn
            .savepoint()
            .map_err(|e| HgbError::Storage(format!("Failed to create savepoint: {}", e)))?;

        sp.execute_batch(migration_sql)
            .map_err(|e| HgbError::Storage(format!("Dry-run migration failed: {}", e)))?;

        // Rollback savepoint cleanly so nothing is committed
        sp.rollback()
            .map_err(|e| HgbError::Storage(format!("Failed to rollback savepoint: {}", e)))?;

        Ok(())
    }

    /// Execute the verified migration in an atomic transaction
    pub fn apply_migration<P: AsRef<Path>>(
        db_path: P,
        migration_sql: &str,
        migration_name: &str,
    ) -> Result<String> {
        let mut conn = Connection::open(db_path.as_ref())
            .map_err(|e| HgbError::Storage(format!("Failed to open DB for migration: {}", e)))?;

        let tx = conn
            .transaction()
            .map_err(|e| HgbError::Storage(format!("Failed to start transaction: {}", e)))?;

        tx.execute_batch(migration_sql)
            .map_err(|e| HgbError::Storage(format!("Migration '{}' failed to apply: {}", migration_name, e)))?;

        tx.commit()
            .map_err(|e| HgbError::Storage(format!("Failed to commit migration: {}", e)))?;

        Ok(format!("Migration '{}' applied successfully", migration_name))
    }
}
