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
- رتّب بالأحدث (date DESC) وحُط LIMIT 50.
- بعد ما تجيب النتائج، جاوب بالعربي باختصار: كام سجل لقيت، وأمثلة (عربية/تاريخ/البند) وأي ملاحظات.
- ماتشغّلش غير SELECT. ممنوع أي تعديل."#;

#[derive(Debug, Deserialize)]
struct AiQuery {
    query: String,
}

fn is_safe_select(sql: &str) -> bool {
    let t = sql.trim_start().to_lowercase();
    if !t.starts_with("select") && !t.starts_with("with") {
        return false;
    }
    let banned = [
        "insert ", "update ", "delete ", "drop ", "alter ", "truncate ", "create ",
        "grant ", "revoke ", "copy ", ";--", "into ",
    ];
    !banned.iter().any(|b| t.contains(b))
}

async fn run_sql(pool: &sqlx::PgPool, query: &str) -> String {
    if !is_safe_select(query) {
        return json!({"error": "only read-only SELECT queries are allowed"}).to_string();
    }
    // Defense in depth: the blacklist above constrains statement *shape*; the
    // READ ONLY transaction makes writes impossible at the database level no
    // matter what the model emits.
    let inner = async {
        let mut tx = pool.begin().await?;
        sqlx::query("SET TRANSACTION READ ONLY").execute(&mut *tx).await?;
        let (rows,): (Value,) = sqlx::query_as(&format!(
            "SELECT COALESCE(jsonb_agg(t), '[]'::jsonb) FROM ({}) t",
            query.trim_end().trim_end_matches(';')
        ))
        .fetch_one(&mut *tx)
        .await?;
        tx.rollback().await?;
        Ok::<Value, sqlx::Error>(rows)
    };
    match inner.await {
        Ok(rows) => rows.to_string(),
        Err(e) => json!({"error": format!("{e}")}).to_string(),
    }
}

#[post("/ai/query")]
async fn ai_query(
    state: web::Data<AppState>,
    // Extracting claims makes authentication mandatory for this endpoint —
    // the model-driven run_sql tool must never be reachable anonymously.
    _claims: AuthClaims,
    body: web::Json<AiQuery>,
) -> ApiResult<HttpResponse> {
    let api_key = state
        .anthropic_api_key
        .as_ref()
        .ok_or_else(|| ApiError::Internal("ANTHROPIC_API_KEY not set".into()))?;
    let pool = state
        .falcon_pool
        .as_ref()
        .ok_or_else(|| ApiError::Internal("Falcon DB not configured".into()))?;

    let client = reqwest::Client::new();
    let tools = json!([{
        "name": "run_sql",
        "description": "Run a single read-only SELECT query against the service database. Returns rows as a JSON array.",
        "input_schema": {
            "type": "object",
            "properties": {"query": {"type": "string", "description": "A single read-only SELECT statement."}},
            "required": ["query"]
        }
    }]);

    // Conversation state — grows as tools run.
    let mut messages: Vec<Value> = vec![json!({"role": "user", "content": body.query})];
    let mut collected_rows: Vec<Value> = Vec::new();
    let mut answer = String::new();

    for _ in 0..6 {
        let req = json!({
            "model": state.ai_model,
            "max_tokens": 4096,
            "system": SYSTEM,
            "tools": tools,
            "messages": messages,
        });
        let resp = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&req)
            .send()
            .await
            .map_err(|e| ApiError::Internal(format!("anthropic request failed: {e}")))?;

        let status = resp.status();
        let v: Value = resp
            .json()
            .await
            .map_err(|e| ApiError::Internal(format!("anthropic bad json: {e}")))?;
        if !status.is_success() {
            return Err(ApiError::Internal(format!("anthropic {}: {}", status, v)));
        }

        let content = v.get("content").and_then(|c| c.as_array()).cloned().unwrap_or_default();
        // Append the assistant turn verbatim (preserves tool_use blocks).
        messages.push(json!({"role": "assistant", "content": content.clone()}));

        // Collect any text + run any tool calls.
        let mut tool_results: Vec<Value> = Vec::new();
        for block in &content {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                        answer.push_str(t);
                    }
                }
                Some("tool_use") => {
                    let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("");
                    let q = block
                        .get("input")
                        .and_then(|i| i.get("query"))
                        .and_then(|q| q.as_str())
                        .unwrap_or("");
                    let result = run_sql(pool, q).await;
                    if let Ok(parsed) = serde_json::from_str::<Value>(&result) {
                        if let Some(arr) = parsed.as_array() {
                            collected_rows = arr.clone();
                        }
                    }
                    tool_results.push(json!({
                        "type": "tool_result", "tool_use_id": id, "content": result
                    }));
                }
                _ => {}
            }
        }

        if v.get("stop_reason").and_then(|s| s.as_str()) == Some("tool_use") && !tool_results.is_empty() {
            messages.push(json!({"role": "user", "content": tool_results}));
            answer.clear(); // keep only the final answer
            continue;
        }
        break;
    }

    Ok(HttpResponse::Ok().json(json!({
        "answer": answer.trim(),
        "rows": collected_rows,
    })))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(ai_query);
}
