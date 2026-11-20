//! AI search over the service history (Falcon `inspection_items` + `service_invoices`).
//!
//! A manual Claude tool-use loop (raw HTTP — no official Rust SDK). Claude gets one
//! read-only `run_sql` tool and a schema-aware, Egyptian-Arabic system prompt that
//! expands a general term (فرامل) into related terms (تيل، دسك، هوبة …). The tool
//! executes SELECT-only queries against Falcon's Postgres and streams rows back to
//! Claude until it produces a final Arabic answer.

use actix_web::{post, web, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::claims::level;
use crate::auth::AuthClaims;
use crate::error::{ApiError, ApiResult};
use crate::handlers::AppState;

const SYSTEM: &str = r#"أنت مساعد بحث لورشة صيانة عربيات (تريلات) في شركة أبيكس. بتساعد الميكانيكي يلاقي سجلّات الصيانة والخدمة.

عندك أداة اسمها run_sql بتشغّل استعلام SELECT واحد للقراءة فقط على قاعدة بيانات Postgres. الجداول:
- service_invoices(id, car_id, date, meter_reading, driver_name, supervisor, operating_region, plate_number)
- inspection_items(id, service_invoice_id, service TEXT, notes TEXT, item_order)  -- كل صف ده بند خدمة اتعمل (بالعربي)

قواعد:
- المستخدم بيسأل بالعامية المصرية عن نوع صيانة أو قطعة. وسّع البحث لكلمات مرادفة/مرتبطة. أمثلة:
  فرامل → 'فرامل','تيل','دسك','هوبة','فحمات'
  تنابير → 'تنابير','ورق شمس','شمعة'
  بلف أو جهاز الهوا → 'بلف','جهاز الهوا','جهاز الهواء','كمبروسور','هواء'
  كاوتش → 'كاوتش','اطار','عجلة','دوبل'
  زيت → 'زيت','اويل','فلتر زيت'
- استخدم ILIKE '%كلمة%' على inspection_items.service (و notes لو مفيد) مع OR لكل الكلمات المرادفة.
- اعمل JOIN مع service_invoices عشان ترجّع plate_number و date و car_id و meter_reading.
- الجدولين فيهم عمود deleted_at: دايماً حُط شرط deleted_at IS NULL على الاتنين (السجلات الممسوحة ماتتحسبش).
- لو السؤال "كام" أو عدد أو إجمالي: استخدم COUNT(*) أو GROUP BY في الاستعلام نفسه (ماتعدّش الصفوف بنفسك، النتيجة بتتقص عند 200 صف).
- لو السؤال عن سجلات: رتّب بالأحدث (date DESC) وحُط LIMIT 50.
- مفيش بيانات تكلفة أو أسعار في الجداول دي؛ لو السؤال عن تكلفة قول كده بوضوح.
- ماتكتبش كلمات زي update أو delete أو insert جوه نص البحث.
- بعد ما تجيب النتائج، جاوب بالعربي باختصار: كام سجل لقيت، وأمثلة (عربية/تاريخ/البند) وأي ملاحظات.
- ماتشغّلش غير SELECT. ممنوع أي تعديل."#;

#[derive(Debug, Deserialize)]
struct AiQuery {
    query: String,
}

/// The only relations the model's SQL may read. Enforced against the query
/// PLAN (see `plan_relations`), not by matching text.
const ALLOWED_RELATIONS: &[&str] = &["service_invoices", "inspection_items"];
/// Rows handed back per query, whatever LIMIT the model wrote.
const MAX_ROWS: i64 = 200;
const MAX_QUESTION_CHARS: usize = 500;

/// Shared outbound client: one connection pool, and a ceiling on how long a
/// request to the model may hang a worker.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("reqwest client build failed")
}

/// Per-user budget: each question costs money and a Falcon DB connection.
#[derive(Clone, Default)]
pub struct RateLimiter(std::sync::Arc<std::sync::Mutex<std::collections::HashMap<i64, Vec<std::time::Instant>>>>);

impl RateLimiter {
    const WINDOW: std::time::Duration = std::time::Duration::from_secs(600);
    const MAX_IN_WINDOW: usize = 30;

    pub fn allow(&self, user_id: i64) -> bool {
        let Ok(mut m) = self.0.lock() else { return true };
        let now = std::time::Instant::now();
        let hits = m.entry(user_id).or_default();
        hits.retain(|t| now.duration_since(*t) < Self::WINDOW);
        if hits.len() >= Self::MAX_IN_WINDOW {
            return false;
        }
        hits.push(now);
        true
    }
}

/// First gate: statement shape. A single SELECT/WITH, no catalog or
/// server-side helper functions. The real boundaries are the plan allowlist
/// and the READ ONLY transaction below.
pub fn is_safe_select(sql: &str) -> bool {
    let t = sql.trim().trim_end_matches(';').to_lowercase();
    if !t.starts_with("select") && !t.starts_with("with") {
        return false;
    }
    if t.contains(';') {
        return false;
    }
    let banned = [
        "pg_", "information_schema", "dblink", "lo_import", "lo_export", "set_config",
        "current_setting", "copy ", "nextval", "setval", "txid_", "query_to_xml",
    ];
    if banned.iter().any(|b| t.contains(b)) {
        return false;
    }
    // Whole-word DML/DDL, whatever whitespace separates it.
    let words: Vec<&str> = t.split(|c: char| !c.is_alphanumeric() && c != '_').collect();
    let dml = ["insert", "update", "delete", "drop", "alter", "truncate", "create", "grant", "revoke", "call", "do"];
    !words.iter().any(|w| dml.contains(w))
}

/// Every relation and function scan a plan touches.
fn plan_relations(plan: &Value, out: &mut Vec<String>, functions: &mut bool) {
    match plan {
        Value::Object(o) => {
            if let Some(r) = o.get("Relation Name").and_then(Value::as_str) {
                out.push(r.to_string());
            }
            if o.get("Node Type").and_then(Value::as_str) == Some("Function Scan") {
                *functions = true;
            }
            for v in o.values() {
                plan_relations(v, out, functions);
            }
        }
        Value::Array(a) => a.iter().for_each(|v| plan_relations(v, out, functions)),
        _ => {}
    }
}

async fn run_sql(pool: &sqlx::PgPool, query: &str) -> String {
    if !is_safe_select(query) {
        return json!({"error": "only a single read-only SELECT is allowed"}).to_string();
    }
    let query = query.trim().trim_end_matches(';');
    let inner = async {
        let mut tx = pool.begin().await?;
        sqlx::query("SET TRANSACTION READ ONLY").execute(&mut *tx).await?;
        sqlx::query("SET LOCAL statement_timeout = '8s'").execute(&mut *tx).await?;
        // Ask Postgres what the query would read. Text matching cannot tell
        // `FROM a, users` from a column called users; the planner can.
        let (plan,): (Value,) =
            sqlx::query_as(&format!("EXPLAIN (FORMAT JSON) {query}")).fetch_one(&mut *tx).await?;
        let mut rels = Vec::new();
        let mut functions = false;
        plan_relations(&plan, &mut rels, &mut functions);
        if functions || rels.iter().any(|r| !ALLOWED_RELATIONS.contains(&r.as_str())) {
            tx.rollback().await?;
            return Ok(json!({"error": "only service_invoices and inspection_items may be queried"}));
        }
        let (rows,): (Value,) = sqlx::query_as(&format!(
            "SELECT COALESCE(jsonb_agg(t), '[]'::jsonb) FROM (SELECT * FROM ({query}) q LIMIT {MAX_ROWS}) t"
        ))
        .fetch_one(&mut *tx)
        .await?;
        tx.rollback().await?;
        Ok::<Value, sqlx::Error>(rows)
    };
    match inner.await {
        Ok(rows) => rows.to_string(),
        Err(e) => {
            // The model needs to know its SQL failed, not the server's internals.
            tracing::debug!(error = %e, "ai run_sql failed");
            let msg = match &e {
                sqlx::Error::Database(db) => db.message().chars().take(200).collect::<String>(),
                _ => "query failed".to_string(),
            };
            json!({ "error": msg }).to_string()
        }
    }
}

#[post("/ai/query")]
async fn ai_query(
    state: web::Data<AppState>,
    claims: AuthClaims,
    body: web::Json<AiQuery>,
) -> ApiResult<HttpResponse> {
    claims.require(level::AI_SEARCH, "AI search")?;
    let question = body.query.trim();
    if question.is_empty() || question.chars().count() > MAX_QUESTION_CHARS {
        return Err(ApiError::BadRequest(format!(
            "the question must be 1 to {MAX_QUESTION_CHARS} characters"
        )));
    }
    let api_key = state
        .anthropic_api_key
        .as_ref()
        .ok_or_else(|| ApiError::Unavailable("AI search is not configured on the server (ANTHROPIC_API_KEY)".into()))?;
    let pool = state
        .falcon_pool
        .as_ref()
        .ok_or_else(|| ApiError::Unavailable("AI search is not configured on the server (service history database)".into()))?;
    if !state.ai_limiter.allow(claims.user_id) {
        return Err(ApiError::TooManyRequests("too many AI questions; try again in a few minutes".into()));
    }

    let tools = json!([{
        "name": "run_sql",
        "description": "Run a single read-only SELECT query against the service database. Returns rows as a JSON array (at most 200 rows).",
        "input_schema": {
            "type": "object",
            "properties": {"query": {"type": "string", "description": "A single read-only SELECT statement."}},
            "required": ["query"]
        }
    }]);

    let mut messages: Vec<Value> = vec![json!({"role": "user", "content": question})];
    let mut collected_rows: Vec<Value> = Vec::new();
    let mut answer = String::new();
    let mut finished = false;

    for _ in 0..6 {
        let req = json!({
            "model": state.ai_model,
            "max_tokens": 4096,
            "system": SYSTEM,
            "tools": tools,
            "messages": messages,
        });
        let resp = state
            .http
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&req)
            .send()
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "anthropic request failed");
                ApiError::Unavailable("the AI service could not be reached".into())
            })?;

        let status = resp.status();
        let v: Value = resp.json().await.map_err(|e| {
            tracing::warn!(error = %e, "anthropic response was not JSON");
            ApiError::Unavailable("the AI service gave an unreadable answer".into())
        })?;
        if !status.is_success() {
            tracing::warn!(status = %status, body = %v, "anthropic error");
            return Err(ApiError::Unavailable(format!("the AI service answered {}", status.as_u16())));
        }

        let content = v.get("content").and_then(|c| c.as_array()).cloned().unwrap_or_default();
        messages.push(json!({"role": "assistant", "content": content.clone()}));

        let mut tool_results: Vec<Value> = Vec::new();
        let mut turn_text = String::new();
        for block in &content {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                        turn_text.push_str(t);
                    }
                }
                Some("tool_use") => {
                    let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("");
                    let q = block.get("input").and_then(|i| i.get("query")).and_then(|q| q.as_str()).unwrap_or("");
                    let result = run_sql(pool, q).await;
                    let parsed = serde_json::from_str::<Value>(&result).unwrap_or(Value::Null);
                    let is_error = parsed.get("error").is_some();
                    if let Some(arr) = parsed.as_array() {
                        // Keep the last query that actually found something.
                        if !arr.is_empty() || collected_rows.is_empty() {
                            collected_rows = arr.clone();
                        }
                    }
                    tool_results.push(json!({
                        "type": "tool_result", "tool_use_id": id, "content": result, "is_error": is_error
                    }));
                }
                _ => {}
            }
        }

        match v.get("stop_reason").and_then(|s| s.as_str()) {
            Some("tool_use") if !tool_results.is_empty() => {
                messages.push(json!({"role": "user", "content": tool_results}));
                continue;
            }
            Some("refusal") => {
                return Err(ApiError::BadRequest("the AI service declined this question".into()));
            }
            _ => {
                answer = turn_text;
                finished = true;
                break;
            }
        }
    }
    if !finished || answer.trim().is_empty() {
        // Never a 200 with an empty answer: the client falls back on failure.
        return Err(ApiError::Unavailable("the AI search did not reach an answer; rephrase and try again".into()));
    }

    Ok(HttpResponse::Ok().json(json!({
        "answer": answer.trim(),
        "rows": collected_rows,
    })))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(ai_query);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_single_select_passes() {
        assert!(is_safe_select("SELECT 1"));
        assert!(is_safe_select("  select * from service_invoices limit 5;"));
        assert!(is_safe_select("WITH c AS (SELECT 1) SELECT * FROM c"));
        for bad in [
            "UPDATE cars SET x=1", "delete from cars", "DROP TABLE cars", "truncate cars",
            "SELECT 1; DELETE FROM cars", "select 1;select 2",
            // whitespace other than a space used to slip past the old blacklist
            "select 1 where exists (delete\nfrom cars returning 1)",
            "with x as (update\tcars set a=1 returning 1) select * from x",
            "select pg_sleep(100)", "select * from pg_shadow", "select current_setting('x')",
            "select * from information_schema.tables", "select set_config('a','b',false)",
        ] {
            assert!(!is_safe_select(bad), "should be refused: {bad}");
        }
    }

    #[test]
    fn ordinary_arabic_searches_are_not_blocked() {
        assert!(is_safe_select(
            "SELECT i.service, s.plate_number, s.date FROM inspection_items i \
             JOIN service_invoices s ON s.id = i.service_invoice_id \
             WHERE i.service ILIKE '%فرامل%' ORDER BY s.date DESC LIMIT 50"
        ));
        assert!(is_safe_select(
            "SELECT count(*) FROM inspection_items i WHERE i.service ILIKE '%زيت%' AND i.deleted_at IS NULL"
        ));
    }

    #[test]
    fn plan_walk_finds_every_relation() {
        let plan = json!([{"Plan": {"Node Type": "Hash Join", "Plans": [
            {"Node Type": "Seq Scan", "Relation Name": "inspection_items"},
            {"Node Type": "Hash", "Plans": [{"Node Type": "Seq Scan", "Relation Name": "users"}]}
        ]}}]);
        let (mut rels, mut f) = (Vec::new(), false);
        plan_relations(&plan, &mut rels, &mut f);
        assert_eq!(rels, vec!["inspection_items", "users"]);
        assert!(!f);
        let plan = json!([{"Plan": {"Node Type": "Function Scan", "Function Name": "generate_series"}}]);
        let (mut rels, mut f) = (Vec::new(), false);
        plan_relations(&plan, &mut rels, &mut f);
        assert!(f);
    }

    #[test]
    fn limiter_caps_a_user_and_not_their_neighbour() {
        let l = RateLimiter::default();
        for _ in 0..RateLimiter::MAX_IN_WINDOW {
            assert!(l.allow(1));
        }
        assert!(!l.allow(1));
        assert!(l.allow(2));
    }

    #[test]
    fn never_panics_on_arbitrary_input() {
        for s in ["", "   ", "سيليكت", "select\0", "\u{1F600}", "with", "select;"] {
            let _ = is_safe_select(s);
        }
    }
}
