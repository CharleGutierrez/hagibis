use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RailsDbAdapter {
    PostgreSQL,
    MySQL,
    SQLite,
    Unknown(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RailsAppInfo {
    pub is_rails: bool,
    pub rails_version: Option<String>,
    pub db_adapter: RailsDbAdapter,
    pub has_routes: bool,
    pub has_rubocop: bool,
    pub has_rspec: bool,
    pub model_count: usize,
    pub migration_count: usize,
    pub detected_gems: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MigrationRiskLevel {
    Safe,
    Warning,
    Hazardous,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationHazard {
    pub line_number: Option<usize>,
    pub risk_level: MigrationRiskLevel,
    pub rule: String,
    pub message: String,
    pub remediation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationSafetyReport {
    pub file_name: String,
    pub overall_risk: MigrationRiskLevel,
    pub hazards: Vec<MigrationHazard>,
    pub safe_to_deploy_zero_downtime: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaffoldField {
    pub name: String,
    pub col_type: String,
    pub is_unique: bool,
    pub is_indexed: bool,
    pub is_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RailsScaffoldResult {
    pub model_name: String,
    pub table_name: String,
    pub model_code: String,
    pub migration_code: String,
    pub controller_code: String,
    pub route_snippet: String,
    pub spec_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NPlusOneIssue {
    pub file_path: String,
    pub line_number: usize,
    pub association: String,
    pub snippet: String,
    pub suggestion: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NPlusOneReport {
    pub issues_found: Vec<NPlusOneIssue>,
    pub total_audited_lines: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedRoute {
    pub verb: String,
    pub path: String,
    pub controller: String,
    pub action: String,
}

pub struct RailsEngine;

impl RailsEngine {
    pub fn new() -> Self {
        Self
    }

    /// Detect if a directory contains a Ruby on Rails application and parse its metadata
    pub fn detect_app(workspace_path: &Path) -> RailsAppInfo {
        let gemfile_path = workspace_path.join("Gemfile");
        let routes_path = workspace_path.join("config/routes.rb");
        let app_models_path = workspace_path.join("app/models");
        let db_migrate_path = workspace_path.join("db/migrate");

        let mut is_rails = routes_path.exists();
        let mut rails_version = None;
        let mut db_adapter = RailsDbAdapter::Unknown("Undetected".to_string());
        let mut detected_gems = Vec::new();
        let mut has_rubocop = workspace_path.join(".rubocop.yml").exists();
        let mut has_rspec = workspace_path.join("spec").exists();

        if gemfile_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&gemfile_path) {
                for line in content.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with('#') {
                        continue;
                    }
                    if trimmed.contains("gem 'rails'") || trimmed.contains("gem \"rails\"") {
                        is_rails = true;
                        if let Some(idx) = trimmed.find('~') {
                            rails_version = Some(trimmed[idx..].trim_matches(|c| c == '\'' || c == '"' || c == ',' || c == ' ').to_string());
                        }
                    }
                    if trimmed.contains("gem 'pg'") || trimmed.contains("gem \"pg\"") {
                        db_adapter = RailsDbAdapter::PostgreSQL;
                        detected_gems.push("pg".to_string());
                    } else if trimmed.contains("gem 'mysql2'") || trimmed.contains("gem \"mysql2\"") {
                        db_adapter = RailsDbAdapter::MySQL;
                        detected_gems.push("mysql2".to_string());
                    } else if trimmed.contains("gem 'sqlite3'") || trimmed.contains("gem \"sqlite3\"") {
                        db_adapter = RailsDbAdapter::SQLite;
                        detected_gems.push("sqlite3".to_string());
                    }
                    if trimmed.contains("rubocop") {
                        has_rubocop = true;
                        detected_gems.push("rubocop".to_string());
                    }
                    if trimmed.contains("rspec-rails") {
                        has_rspec = true;
                        detected_gems.push("rspec-rails".to_string());
                    }
                    if trimmed.contains("bullet") {
                        detected_gems.push("bullet".to_string());
                    }
                    if trimmed.contains("sidekiq") {
                        detected_gems.push("sidekiq".to_string());
                    }
                }
            }
        }

        let model_count = if app_models_path.exists() {
            std::fs::read_dir(&app_models_path)
                .map(|entries| {
                    entries
                        .filter_map(std::result::Result::ok)
                        .filter(|e| e.path().extension().map_or(false, |ext| ext == "rb"))
                        .count()
                })
                .unwrap_or(0)
        } else {
            0
        };

        let migration_count = if db_migrate_path.exists() {
            std::fs::read_dir(&db_migrate_path)
                .map(|entries| {
                    entries
                        .filter_map(std::result::Result::ok)
                        .filter(|e| e.path().extension().map_or(false, |ext| ext == "rb"))
                        .count()
                })
                .unwrap_or(0)
        } else {
            0
        };

        RailsAppInfo {
            is_rails,
            rails_version,
            db_adapter,
            has_routes: routes_path.exists(),
            has_rubocop,
            has_rspec,
            model_count,
            migration_count,
            detected_gems,
        }
    }

    /// Zero-downtime ActiveRecord migration linter. Detects locks and table rewrite hazards.
    pub fn lint_migration(file_name: &str, migration_code: &str) -> MigrationSafetyReport {
        let mut hazards = Vec::new();

        for (idx, line) in migration_code.lines().enumerate() {
            let line_number = idx + 1;
            let trimmed = line.trim();

            if trimmed.starts_with('#') {
                continue;
            }

            // Hazard 1: add_index without algorithm: :concurrently
            if trimmed.contains("add_index") && !trimmed.contains("algorithm: :concurrently") {
                hazards.push(MigrationHazard {
                    line_number: Some(line_number),
                    risk_level: MigrationRiskLevel::Hazardous,
                    rule: "CONCURRENT_INDEX_REQUIRED".to_string(),
                    message: "Adding an index without `algorithm: :concurrently` takes an ACCESS EXCLUSIVE lock on PostgreSQL, blocking reads and writes.".to_string(),
                    remediation: "Add `algorithm: :concurrently` and disable DDL transactions via `disable_ddl_transaction!`".to_string(),
                });
            }

            // Hazard 2: add_column with default value on old PostgreSQL / MySQL
            if trimmed.contains("add_column") && trimmed.contains("default:") {
                hazards.push(MigrationHazard {
                    line_number: Some(line_number),
                    risk_level: MigrationRiskLevel::Warning,
                    rule: "ADD_COLUMN_WITH_DEFAULT".to_string(),
                    message: "Adding a column with a default value on large tables can rewrite the entire table and cause downtime.".to_string(),
                    remediation: "Add column without default first, backfill in batches, then set default separately.".to_string(),
                });
            }

            // Hazard 3: remove_column without ignored_columns first
            if trimmed.contains("remove_column") || trimmed.contains("remove_columns") {
                hazards.push(MigrationHazard {
                    line_number: Some(line_number),
                    risk_level: MigrationRiskLevel::Hazardous,
                    rule: "REMOVE_COLUMN_WITHOUT_IGNORE".to_string(),
                    message: "Removing a column while old application code is still running causes ActiveRecord::StatementInvalid errors.".to_string(),
                    remediation: "Step 1: Set `self.ignored_columns = [...]` in model and deploy. Step 2: Remove column in a subsequent migration.".to_string(),
                });
            }

            // Hazard 4: rename_column or rename_table
            if trimmed.contains("rename_column") || trimmed.contains("rename_table") {
                hazards.push(MigrationHazard {
                    line_number: Some(line_number),
                    risk_level: MigrationRiskLevel::Hazardous,
                    rule: "RENAME_COLUMN_OR_TABLE".to_string(),
                    message: "Renaming columns or tables causes immediate downtime during rolling deployments.".to_string(),
                    remediation: "Add new column, sync data with database trigger or callback, deploy code using new column, then drop old column.".to_string(),
                });
            }

            // Hazard 5: change_column without null check
            if trimmed.contains("change_column") && !trimmed.contains("change_column_null") {
                hazards.push(MigrationHazard {
                    line_number: Some(line_number),
                    risk_level: MigrationRiskLevel::Warning,
                    rule: "CHANGE_COLUMN_TYPE".to_string(),
                    message: "change_column can lock the table and may cause type casting failures on existing rows.".to_string(),
                    remediation: "Add a new column with the desired type, dual-write, backfill, and swap.".to_string(),
                });
            }

            // Hazard 6: add_reference without index: false or concurrent index
            if trimmed.contains("add_reference") || trimmed.contains("add_belongs_to") {
                if !trimmed.contains("index: false") && !trimmed.contains("concurrently") {
                    hazards.push(MigrationHazard {
                        line_number: Some(line_number),
                        risk_level: MigrationRiskLevel::Warning,
                        rule: "REFERENCE_INDEX_LOCK".to_string(),
                        message: "add_reference automatically generates an index that blocks the table unless index: false is passed.".to_string(),
                        remediation: "Use `add_reference :table, :target, index: false` and add the index concurrently in a separate step.".to_string(),
                    });
                }
            }
        }

        let overall_risk = if hazards.iter().any(|h| h.risk_level == MigrationRiskLevel::Hazardous) {
            MigrationRiskLevel::Hazardous
        } else if hazards.iter().any(|h| h.risk_level == MigrationRiskLevel::Warning) {
            MigrationRiskLevel::Warning
        } else {
            MigrationRiskLevel::Safe
        };

        MigrationSafetyReport {
            file_name: file_name.to_string(),
            overall_risk: overall_risk.clone(),
            safe_to_deploy_zero_downtime: overall_risk == MigrationRiskLevel::Safe,
            hazards,
        }
    }

    /// Synthesizes a complete, production-grade Rails scaffold (Model, Migration, Controller, Spec, Route)
    pub fn generate_scaffold(model_name: &str, fields: &[ScaffoldField]) -> RailsScaffoldResult {
        let table_name = format!("{}s", model_name.to_lowercase());
        let singular = model_name.to_lowercase();

        // Model Code
        let mut model_validations = Vec::new();
        for field in fields {
            if field.is_required {
                model_validations.push(format!("  validates :{}, presence: true", field.name));
            }
            if field.is_unique {
                model_validations.push(format!("  validates :{}, uniqueness: true", field.name));
            }
        }
        let validations_str = if model_validations.is_empty() {
            "".to_string()
        } else {
            format!("\n{}\n", model_validations.join("\n"))
        };

        let model_code = format!(
            "# frozen_string_literal: true\n\nclass {} < ApplicationRecord{}\nend\n",
            model_name, validations_str
        );

        // Migration Code
        let mut migration_cols = Vec::new();
        let mut migration_indexes = Vec::new();
        for field in fields {
            let col_line = format!("      t.{} :{}", field.col_type, field.name);
            migration_cols.push(col_line);
            if field.is_unique {
                migration_indexes.push(format!(
                    "    add_index :{}, :{}, unique: true",
                    table_name, field.name
                ));
            } else if field.is_indexed {
                migration_indexes.push(format!("    add_index :{}, :{}", table_name, field.name));
            }
        }

        let indexes_block = if migration_indexes.is_empty() {
            "".to_string()
        } else {
            format!("\n\n{}", migration_indexes.join("\n"))
        };

        let migration_code = format!(
            "# frozen_string_literal: true\n\nclass Create{} < ActiveRecord::Migration[7.1]\n  def change\n    create_table :{} do |t|\n{}\n      t.timestamps\n    end{}\n  end\nend\n",
            format!("{}s", model_name),
            table_name,
            migration_cols.join("\n"),
            indexes_block
        );

        // Controller Code with Strong Parameters
        let permitted_params: Vec<String> = fields.iter().map(|f| format!(":{}", f.name)).collect();
        let controller_code = format!(
            r#"# frozen_string_literal: true

class {}Controller < ApplicationController
  before_action :set_{}, only: %i[show update destroy]

  def index
    @{} = {}.all
    render json: @{}
  end

  def show
    render json: @{}
  end

  def create
    @{} = {}.new({}_params)
    if @{}.save
      render json: @{}, status: :created
    else
      render json: @{}.errors, status: :unprocessable_entity
    end
  end

  def update
    if @{}.update({}_params)
      render json: @{}
    else
      render json: @{}.errors, status: :unprocessable_entity
    end
  end

  def destroy
    @{}.destroy
    head :no_content
  end

  private

  def set_{}
    @{} = {}.find(params[:id])
  end

  def {}_params
    params.require(:{}).permit({})
  end
end
"#,
            format!("{}s", model_name),
            singular,
            table_name,
            model_name,
            table_name,
            singular,
            singular,
            model_name,
            singular,
            singular,
            singular,
            singular,
            singular,
            singular,
            singular,
            singular,
            singular,
            singular,
            singular,
            model_name,
            singular,
            singular,
            permitted_params.join(", ")
        );

        // Route snippet
        let route_snippet = format!("resources :{}", table_name);

        // RSpec code
        let spec_code = format!(
            "# frozen_string_literal: true\n\nrequire 'rails_helper'\n\nRSpec.describe {}, type: :model do\n  describe 'validations' do\n{}\n  end\nend\n",
            model_name,
            fields
                .iter()
                .filter(|f| f.is_required)
                .map(|f| format!("    it {{ should validate_presence_of(:{}) }}", f.name))
                .collect::<Vec<_>>()
                .join("\n")
        );

        RailsScaffoldResult {
            model_name: model_name.to_string(),
            table_name,
            model_code,
            migration_code,
            controller_code,
            route_snippet,
            spec_code,
        }
    }

    /// Audits code snippets for potential N+1 query antipatterns
    pub fn audit_n_plus_one(file_path: &str, content: &str) -> NPlusOneReport {
        let mut issues = Vec::new();
        let lines: Vec<&str> = content.lines().collect();

        for (idx, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if (trimmed.contains(".each do |") || trimmed.contains(".map do |") || trimmed.contains(".each { |"))
                && !trimmed.contains("includes")
                && !trimmed.contains("preload")
                && !trimmed.contains("eager_load")
            {
                let var_name = if let Some(start) = trimmed.find('|') {
                    if let Some(end) = trimmed[start + 1..].find('|') {
                        trimmed[start + 1..start + 1 + end].trim()
                    } else {
                        ""
                    }
                } else {
                    ""
                };

                if !var_name.is_empty() {
                    for next_idx in (idx + 1)..(idx + 10).min(lines.len()) {
                        let next_line = lines[next_idx].trim();
                        let target_call = format!("{}.", var_name);
                        if next_line.contains(&target_call)
                            && (next_line.contains(".comments")
                                || next_line.contains(".posts")
                                || next_line.contains(".profile")
                                || next_line.contains(".author")
                                || next_line.contains(".organization")
                                || next_line.contains(".account"))
                        {
                            issues.push(NPlusOneIssue {
                                file_path: file_path.to_string(),
                                line_number: next_idx + 1,
                                association: next_line.to_string(),
                                snippet: format!("{}\n  {}", trimmed, next_line),
                                suggestion: format!(
                                    "Eager load association with `.includes(:association)` before calling `{}` to prevent N+1 queries.",
                                    trimmed
                                ),
                            });
                            break;
                        }
                    }
                }
            }
        }

        NPlusOneReport {
            issues_found: issues,
            total_audited_lines: lines.len(),
        }
    }

    /// Parses routes from a Rails config/routes.rb content
    pub fn parse_routes(routes_content: &str) -> Vec<ParsedRoute> {
        let mut routes = Vec::new();

        for line in routes_content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') || trimmed.is_empty() {
                continue;
            }

            if trimmed.starts_with("resources :") {
                let resource_name = trimmed
                    .trim_start_matches("resources :")
                    .split(',')
                    .next()
                    .unwrap_or("")
                    .trim();
                let ctrl = resource_name.to_string();
                routes.push(ParsedRoute {
                    verb: "GET".to_string(),
                    path: format!("/{}", resource_name),
                    controller: ctrl.clone(),
                    action: "index".to_string(),
                });
                routes.push(ParsedRoute {
                    verb: "POST".to_string(),
                    path: format!("/{}", resource_name),
                    controller: ctrl.clone(),
                    action: "create".to_string(),
                });
                routes.push(ParsedRoute {
                    verb: "GET".to_string(),
                    path: format!("/{}/:id", resource_name),
                    controller: ctrl.clone(),
                    action: "show".to_string(),
                });
                routes.push(ParsedRoute {
                    verb: "PATCH".to_string(),
                    path: format!("/{}/:id", resource_name),
                    controller: ctrl.clone(),
                    action: "update".to_string(),
                });
                routes.push(ParsedRoute {
                    verb: "DELETE".to_string(),
                    path: format!("/{}/:id", resource_name),
                    controller: ctrl,
                    action: "destroy".to_string(),
                });
            }

            for verb in &["get", "post", "put", "patch", "delete"] {
                if trimmed.starts_with(verb) {
                    let parts: Vec<&str> = trimmed.split("to:").collect();
                    if parts.len() == 2 {
                        let path_part = parts[0]
                            .trim_start_matches(verb)
                            .trim()
                            .trim_matches(|c| c == '\'' || c == '"' || c == ',');
                        let dest = parts[1]
                            .trim()
                            .trim_matches(|c| c == '\'' || c == '"' || c == ',');
                        let dest_parts: Vec<&str> = dest.split('#').collect();
                        let controller = dest_parts.get(0).copied().unwrap_or("application").to_string();
                        let action = dest_parts.get(1).copied().unwrap_or("index").to_string();

                        routes.push(ParsedRoute {
                            verb: verb.to_uppercase(),
                            path: path_part.to_string(),
                            controller,
                            action,
                        });
                    }
                }
            }
        }

        routes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lint_migration_hazards() {
        let migration_bad = r#"
class AddIndexAndColumnToUsers < ActiveRecord::Migration[7.1]
  def change
    add_index :users, :email
    add_column :users, :status, :string, default: "active"
    remove_column :users, :old_token
  end
end
"#;
        let report = RailsEngine::lint_migration("20260928_bad.rb", migration_bad);
        assert_eq!(report.overall_risk, MigrationRiskLevel::Hazardous);
        assert!(!report.safe_to_deploy_zero_downtime);
        assert_eq!(report.hazards.len(), 3);
        assert!(report.hazards.iter().any(|h| h.rule == "CONCURRENT_INDEX_REQUIRED"));
        assert!(report.hazards.iter().any(|h| h.rule == "ADD_COLUMN_WITH_DEFAULT"));
        assert!(report.hazards.iter().any(|h| h.rule == "REMOVE_COLUMN_WITHOUT_IGNORE"));
    }

    #[test]
    fn test_lint_migration_safe() {
        let migration_safe = r#"
class AddSafeIndexToUsers < ActiveRecord::Migration[7.1]
  disable_ddl_transaction!

  def change
    add_index :users, :email, algorithm: :concurrently
  end
end
"#;
        let report = RailsEngine::lint_migration("20260928_safe.rb", migration_safe);
        assert_eq!(report.overall_risk, MigrationRiskLevel::Safe);
        assert!(report.safe_to_deploy_zero_downtime);
        assert!(report.hazards.is_empty());
    }

    #[test]
    fn test_generate_scaffold() {
        let fields = vec![
            ScaffoldField {
                name: "email".to_string(),
                col_type: "string".to_string(),
                is_unique: true,
                is_indexed: true,
                is_required: true,
            },
            ScaffoldField {
                name: "name".to_string(),
                col_type: "string".to_string(),
                is_unique: false,
                is_indexed: false,
                is_required: true,
            },
        ];
        let scaffold = RailsEngine::generate_scaffold("User", &fields);
        assert_eq!(scaffold.model_name, "User");
        assert_eq!(scaffold.table_name, "users");
        assert!(scaffold.model_code.contains("class User < ApplicationRecord"));
        assert!(scaffold.model_code.contains("validates :email, uniqueness: true"));
        assert!(scaffold.migration_code.contains("create_table :users"));
        assert!(scaffold.controller_code.contains("class UsersController < ApplicationController"));
        assert!(scaffold.controller_code.contains("params.require(:user).permit(:email, :name)"));
        assert_eq!(scaffold.route_snippet, "resources :users");
    }

    #[test]
    fn test_audit_n_plus_one() {
        let controller_code = r#"
class PostsController < ApplicationController
  def index
    @posts = Post.all
    @posts.each do |post|
      puts post.comments.count
    end
  end
end
"#;
        let report = RailsEngine::audit_n_plus_one("app/controllers/posts_controller.rb", controller_code);
        assert_eq!(report.issues_found.len(), 1);
        assert!(report.issues_found[0].suggestion.contains("includes"));
    }

    #[test]
    fn test_parse_routes() {
        let routes_txt = r#"
Rails.application.routes.draw do
  resources :users
  get '/health', to: 'status#show'
  post '/auth/login', to: 'sessions#create'
end
"#;
        let routes = RailsEngine::parse_routes(routes_txt);
        assert_eq!(routes.len(), 7);
        assert_eq!(routes[0].path, "/users");
        assert_eq!(routes[5].path, "/health");
        assert_eq!(routes[5].controller, "status");
        assert_eq!(routes[5].action, "show");
    }
}
