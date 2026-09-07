//! Security seam tests (slice E, step 1 — test-first).
//!
//! FakeDatabaseRepository implements the new DatabaseRepository trait,
//! records the ValidatedSql it receives, and returns a canned QueryResult.
//! The agent execute path must deliver validator-approved SQL to the DB,
//! and blocked SQL must never reach the DB.

use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use sql_agent::{
    agent::Agent,
    config::{AuditConfig, Config, DbConfig, LimitsConfig, LlmConfig, PolicyConfig},
    database::{
        schema::{ColumnMatch, TableDetail},
        ColumnInfo, DatabaseRepository, QueryResult, TableInfo,
    },
    llm::{LlmProvider, Message, ToolCall, ToolDefinition, ToolFunction},
    security::{SecurityPolicy, SqlValidator, ValidatedSql},
};

fn test_config() -> Config {
    Config {
        db: DbConfig {
            host: "localhost".into(),
            port: 1433,
            name: "TestDB".into(),
            user: "user".into(),
            password: "pass".into(),
            trust_cert: true,
        },
        llm: LlmConfig {
            provider: "ollama".into(),
            model: String::new(),
            api_key: String::new(),
            base_url: String::new(),
            ollama_url: "http://127.0.0.1:11434".into(),
            ollama_model: "qwen3:4b".into(),
            timeout_s: 120,
            connect_timeout_s: 5,
            temperature: 0.0,
            max_retries: 3,
        },
        policy: PolicyConfig {
            allowed_tables: vec!["dbo.entradaLote".into()],
            block_sensitive_columns: true,
            block_comments: true,
            allow_cte: true,
            allow_system_tables: false,
        },
        limits: LimitsConfig {
            max_steps: 8,
            max_sql_length: 10_000,
            max_rows: 100,
            schema_cache_s: 300,
            query_timeout_s: 30,
            max_concurrent_queries: 1,
            max_joins: 5,
            max_subqueries: 5,
            max_schema_results: 20,
            max_tool_result_chars: 20_000,
            max_tool_calls_per_step: 10,
        },
        audit: AuditConfig {
            enabled: false,
            path: "logs/test-audit.jsonl".into(),
            capture_sql: false,
        },
        verbose: false,
    }
}

fn test_policy() -> SecurityPolicy {
    SecurityPolicy {
        max_sql_length: 10_000,
        allowed_tables: vec!["dbo.entradaLote".into()],
        block_sensitive_columns: true,
        block_comments: true,
        allow_cte: true,
        allow_system_tables: false,
        max_joins: 5,
        max_subqueries: 5,
    }
}

struct FakeDatabaseRepository {
    received: Mutex<Vec<String>>,
}

impl FakeDatabaseRepository {
    fn new() -> Self {
        Self {
            received: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl DatabaseRepository for FakeDatabaseRepository {
    async fn ping(&self) -> anyhow::Result<(String, String)> {
        Ok(("FakeDB".into(), "fake_login".into()))
    }

    async fn list_tables(&self) -> anyhow::Result<Vec<TableInfo>> {
        Ok(vec![])
    }

    async fn search_columns(&self, _term: &str) -> anyhow::Result<Vec<ColumnMatch>> {
        Ok(vec![])
    }

    async fn describe_table_full(&self, _table: &str) -> anyhow::Result<TableDetail> {
        Ok(TableDetail {
            columns: vec![],
            primary_keys: vec![],
            foreign_keys: vec![],
            view_definition: None,
            sample_rows: vec![],
            row_count: 0,
        })
    }

    async fn describe_table(&self, _table: &str) -> anyhow::Result<Vec<ColumnInfo>> {
        Ok(vec![])
    }

    async fn execute_read(&self, validated: ValidatedSql) -> anyhow::Result<QueryResult> {
        self.received
            .lock()
            .unwrap()
            .push(validated.as_str().to_owned());
        Ok(QueryResult {
            row_count: 1,
            truncated: false,
            rows: vec![serde_json::json!({"id": 1})],
        })
    }
}

struct FakeLlm {
    calls: Mutex<usize>,
    sql: String,
}

impl FakeLlm {
    fn new(sql: String) -> Self {
        Self {
            calls: Mutex::new(0),
            sql,
        }
    }
}

#[async_trait]
impl LlmProvider for FakeLlm {
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _verbose: bool,
    ) -> anyhow::Result<Message> {
        let mut n = self.calls.lock().unwrap();
        *n += 1;
        if *n == 1 {
            Ok(Message {
                role: "assistant".into(),
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: Some("call1".into()),
                    function: ToolFunction {
                        name: "execute_read_query".into(),
                        arguments: serde_json::json!({"sql": self.sql}),
                    },
                }],
                name: None,
                tool_call_id: None,
            })
        } else {
            Ok(Message {
                role: "assistant".into(),
                content: "done".into(),
                tool_calls: vec![],
                name: None,
                tool_call_id: None,
            })
        }
    }
}

#[test]
fn validated_sql_only_via_validator() {
    let v = SqlValidator::new(test_policy());
    let ok_sql = "SELECT TOP 10 * FROM dbo.entradaLote";
    let validated = v.validate(ok_sql).expect("allowed SQL must validate");
    assert_eq!(validated.as_str(), ok_sql);
    assert_eq!(validated.into_string(), ok_sql.to_owned());
    // Blocked SQL never yields a ValidatedSql.
    assert!(v.validate("DELETE FROM dbo.entradaLote").is_err());
}

#[tokio::test]
async fn agent_execute_path_delivers_validated_sql_to_db() {
    let sql = "SELECT TOP 10 * FROM dbo.entradaLote";
    let fake_db = Arc::new(FakeDatabaseRepository::new());
    let fake_llm: Arc<dyn LlmProvider> = Arc::new(FakeLlm::new(sql.to_owned()));
    let agent = Agent::with_repository(test_config(), fake_llm, fake_db.clone());
    let answer = agent.run("pregunta").await.expect("run must succeed");
    assert_eq!(answer, "done");
    let received = fake_db.received.lock().unwrap();
    assert_eq!(received.len(), 1, "exactly one validated query must reach the DB");
    assert_eq!(received[0], sql);
}

#[tokio::test]
async fn blocked_sql_never_reaches_db() {
    let sql = "DELETE FROM dbo.entradaLote";
    let fake_db = Arc::new(FakeDatabaseRepository::new());
    let fake_llm: Arc<dyn LlmProvider> = Arc::new(FakeLlm::new(sql.to_owned()));
    let agent = Agent::with_repository(test_config(), fake_llm, fake_db.clone());
    let _ = agent.run("pregunta").await.expect("run must still answer");
    let received = fake_db.received.lock().unwrap();
    assert!(
        received.is_empty(),
        "blocked SQL must never reach the DB, got: {received:?}"
    );
}
