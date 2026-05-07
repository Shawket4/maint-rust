use actix_web::{delete, get, post, put, web, HttpResponse, Scope};
use serde_json::json;
use uuid::Uuid;

use crate::auth::AuthClaims;
use crate::db::PgPool;
use crate::error::{ApiError, ApiResult};
use crate::models::chassis::{
    AxleRow, CreateAxleInput, CreateLayoutInput, CreateSpareInput, LayoutRow, PositionRow,
    UpdateAxleInput, UpdateLayoutInput,
};
use crate::services::chassis_builder::{positions_for_axle, spare_code};

// ---------- Layouts ----------

#[post("/chassis/layouts")]
async fn create_layout(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    body: web::Json<CreateLayoutInput>,
) -> ApiResult<HttpResponse> {
    let inp = body.into_inner();
    let id = inp.id.unwrap_or_else(Uuid::new_v4);
    let row: LayoutRow = sqlx::query_as(
        r#"INSERT INTO chassis_layouts (id, vehicle_id, name_ar, name_en, created_by_user_id, updated_by_user_id)
           VALUES ($1, $2, $3, $4, $5, $5)
           RETURNING *"#,
    )
    .bind(id)
    .bind(inp.vehicle_id)
    .bind(&inp.name_ar)
    .bind(&inp.name_en)
    .bind(claims.user_id)
    .fetch_one(pool.get_ref())
    .await?;
    Ok(HttpResponse::Created().json(row))
}

#[get("/chassis/layouts")]
async fn list_layouts(pool: web::Data<PgPool>) -> ApiResult<HttpResponse> {
    let rows: Vec<LayoutRow> = sqlx::query_as(
        "SELECT * FROM chassis_layouts WHERE deleted_at IS NULL ORDER BY created_at DESC",
    )
    .fetch_all(pool.get_ref())
    .await?;
    Ok(HttpResponse::Ok().json(rows))
}

#[get("/chassis/layouts/{id}")]
async fn get_layout(
    pool: web::Data<PgPool>,
    path: web::Path<Uuid>,
) -> ApiResult<HttpResponse> {
    let id = path.into_inner();
    let row: Option<LayoutRow> = sqlx::query_as(
        "SELECT * FROM chassis_layouts WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool.get_ref())
    .await?;
    match row {
        Some(r) => Ok(HttpResponse::Ok().json(r)),
        None => Err(ApiError::NotFound(format!("layout {id}"))),
    }
}

#[put("/chassis/layouts/{id}")]
async fn update_layout(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<Uuid>,
    body: web::Json<UpdateLayoutInput>,
) -> ApiResult<HttpResponse> {
    let id = path.into_inner();
    let upd = body.into_inner();

    let mut tx = pool.begin().await?;
    let curr: Option<LayoutRow> = sqlx::query_as(
        "SELECT * FROM chassis_layouts WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let curr = curr.ok_or_else(|| ApiError::NotFound(format!("layout {id}")))?;

    if curr.sync_version != upd.sync_version {
        let server_row =
            sqlx::query_scalar::<_, serde_json::Value>("SELECT to_jsonb(t) FROM chassis_layouts t WHERE id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        return Err(ApiError::SyncConflict {
            entity_id: id.to_string(),
            server_row,
        });
    }

    let new_ar = match upd.name_ar {
        Some(opt) => opt,
        None => curr.name_ar.clone(),
    };
    let new_en = match upd.name_en {
        Some(opt) => opt,
        None => curr.name_en.clone(),
    };

    let row: LayoutRow = sqlx::query_as(
        r#"UPDATE chassis_layouts SET
              name_ar = $1, name_en = $2,
              updated_at = now(), updated_by_user_id = $3,
              sync_version = sync_version + 1
           WHERE id = $4
           RETURNING *"#,
    )
    .bind(new_ar)
    .bind(new_en)
    .bind(claims.user_id)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(row))
}

#[delete("/chassis/layouts/{id}")]
async fn delete_layout(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<Uuid>,
) -> ApiResult<HttpResponse> {
    let id = path.into_inner();
    let res = sqlx::query(
        r#"UPDATE chassis_layouts SET deleted_at = now(), updated_at = now(),
                  updated_by_user_id = $1, sync_version = sync_version + 1
           WHERE id = $2 AND deleted_at IS NULL"#,
    )
    .bind(claims.user_id)
    .bind(id)
    .execute(pool.get_ref())
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("layout {id}")));
    }
    Ok(HttpResponse::NoContent().finish())
}

// ---------- Axles ----------

#[post("/chassis/axles")]
async fn create_axle(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    body: web::Json<CreateAxleInput>,
) -> ApiResult<HttpResponse> {
    let inp = body.into_inner();
    if inp.section_index < 1 {
        return Err(ApiError::BadRequest("section_index must be >= 1".into()));
    }

    let mut tx = pool.begin().await?;

    // Verify layout exists
    let _layout: LayoutRow = sqlx::query_as(
        "SELECT * FROM chassis_layouts WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(inp.layout_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::BadRequest(format!("layout {} not found", inp.layout_id)))?;

    let axle_id = inp.id.unwrap_or_else(Uuid::new_v4);
    let axle: AxleRow = sqlx::query_as(
        r#"INSERT INTO chassis_axles (
              id, layout_id, section, section_index, axle_type,
              label_ar, label_en, is_steering, is_lifted,
              created_by_user_id, updated_by_user_id
           ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$10)
           RETURNING *"#,
    )
    .bind(axle_id)
    .bind(inp.layout_id)
    .bind(inp.section)
    .bind(inp.section_index)
    .bind(inp.axle_type)
    .bind(&inp.label_ar)
    .bind(&inp.label_en)
    .bind(inp.is_steering)
    .bind(inp.is_lifted)
    .bind(claims.user_id)
    .fetch_one(&mut *tx)
    .await?;

    // Auto-create positions per §9.2. Server is the only source of position_code.
    let specs = positions_for_axle(inp.section, inp.section_index, inp.axle_type);
    let mut created_positions = Vec::with_capacity(specs.len());
    for s in specs {
        let pos: PositionRow = sqlx::query_as(
            r#"INSERT INTO chassis_positions (
                  id, layout_id, axle_id, side, depth, is_spare, position_code,
                  created_by_user_id, updated_by_user_id
               ) VALUES (gen_random_uuid(), $1, $2, $3, $4, FALSE, $5, $6, $6)
               RETURNING *"#,
        )
        .bind(inp.layout_id)
        .bind(axle_id)
        .bind(s.side)
        .bind(s.depth)
        .bind(&s.code)
        .bind(claims.user_id)
        .fetch_one(&mut *tx)
        .await?;
        created_positions.push(pos);
    }

    // Bump layout updated_at + sync_version so clients re-pull
    sqlx::query(
        r#"UPDATE chassis_layouts SET updated_at = now(),
                  updated_by_user_id = $1, sync_version = sync_version + 1
           WHERE id = $2"#,
    )
    .bind(claims.user_id)
    .bind(inp.layout_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(HttpResponse::Created().json(json!({
        "axle": axle,
        "positions": created_positions,
    })))
}

#[derive(serde::Deserialize)]
struct AxleListQuery {
    layout_id: Option<Uuid>,
}

#[get("/chassis/axles")]
async fn list_axles(
    pool: web::Data<PgPool>,
    q: web::Query<AxleListQuery>,
) -> ApiResult<HttpResponse> {
    let rows: Vec<AxleRow> = match q.layout_id {
        Some(lid) => {
            sqlx::query_as(
                "SELECT * FROM chassis_axles WHERE deleted_at IS NULL AND layout_id = $1 ORDER BY section, section_index",
            )
            .bind(lid)
            .fetch_all(pool.get_ref())
            .await?
        }
        None => {
            sqlx::query_as(
                "SELECT * FROM chassis_axles WHERE deleted_at IS NULL ORDER BY layout_id, section, section_index LIMIT 1000",
            )
            .fetch_all(pool.get_ref())
            .await?
        }
    };
    Ok(HttpResponse::Ok().json(rows))
}

#[put("/chassis/axles/{id}")]
async fn update_axle(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<Uuid>,
    body: web::Json<UpdateAxleInput>,
) -> ApiResult<HttpResponse> {
    let id = path.into_inner();
    let upd = body.into_inner();

    let mut tx = pool.begin().await?;
    let curr: Option<AxleRow> = sqlx::query_as(
        "SELECT * FROM chassis_axles WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let curr = curr.ok_or_else(|| ApiError::NotFound(format!("axle {id}")))?;

    if curr.sync_version != upd.sync_version {
        let server_row = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT to_jsonb(t) FROM chassis_axles t WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        return Err(ApiError::SyncConflict {
            entity_id: id.to_string(),
            server_row,
        });
    }

    let new_section_index = upd.section_index.unwrap_or(curr.section_index);
    let new_axle_type = upd.axle_type.unwrap_or(curr.axle_type);
    let new_label_ar = match upd.label_ar {
        Some(o) => o,
        None => curr.label_ar.clone(),
    };
    let new_label_en = match upd.label_en {
        Some(o) => o,
        None => curr.label_en.clone(),
    };
    let new_is_steering = upd.is_steering.unwrap_or(curr.is_steering);
    let new_is_lifted = upd.is_lifted.unwrap_or(curr.is_lifted);

    let row: AxleRow = sqlx::query_as(
        r#"UPDATE chassis_axles SET
             section_index = $1, axle_type = $2,
             label_ar = $3, label_en = $4,
             is_steering = $5, is_lifted = $6,
             updated_at = now(), updated_by_user_id = $7,
             sync_version = sync_version + 1
           WHERE id = $8
           RETURNING *"#,
    )
    .bind(new_section_index)
    .bind(new_axle_type)
    .bind(new_label_ar)
    .bind(new_label_en)
    .bind(new_is_steering)
    .bind(new_is_lifted)
    .bind(claims.user_id)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;

    // If axle_type or section_index changed, regenerate position codes for the axle.
    let axle_type_changed = new_axle_type != curr.axle_type;
    let section_index_changed = new_section_index != curr.section_index;
    if axle_type_changed || section_index_changed {
        // Soft-delete existing non-spare positions for this axle, then recreate.
        sqlx::query(
            r#"UPDATE chassis_positions
               SET deleted_at = now(),
                   updated_at = now(),
                   updated_by_user_id = $1,
                   sync_version = sync_version + 1
               WHERE axle_id = $2 AND deleted_at IS NULL"#,
        )
        .bind(claims.user_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;

        let specs = positions_for_axle(curr.section, new_section_index, new_axle_type);
        for s in specs {
            sqlx::query(
                r#"INSERT INTO chassis_positions (
                     id, layout_id, axle_id, side, depth, is_spare, position_code,
                     created_by_user_id, updated_by_user_id
                   ) VALUES (gen_random_uuid(), $1, $2, $3, $4, FALSE, $5, $6, $6)"#,
            )
            .bind(curr.layout_id)
            .bind(id)
            .bind(s.side)
            .bind(s.depth)
            .bind(&s.code)
            .bind(claims.user_id)
            .execute(&mut *tx)
            .await?;
        }
    }

    tx.commit().await?;
    Ok(HttpResponse::Ok().json(row))
}

#[delete("/chassis/axles/{id}")]
async fn delete_axle(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<Uuid>,
) -> ApiResult<HttpResponse> {
    let id = path.into_inner();

    let mut tx = pool.begin().await?;
    let res = sqlx::query(
        r#"UPDATE chassis_axles SET deleted_at = now(), updated_at = now(),
                  updated_by_user_id = $1, sync_version = sync_version + 1
           WHERE id = $2 AND deleted_at IS NULL"#,
    )
    .bind(claims.user_id)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::NotFound(format!("axle {id}")));
    }
    sqlx::query(
        r#"UPDATE chassis_positions SET deleted_at = now(), updated_at = now(),
                  updated_by_user_id = $1, sync_version = sync_version + 1
           WHERE axle_id = $2 AND deleted_at IS NULL"#,
    )
    .bind(claims.user_id)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::NoContent().finish())
}

// ---------- Spare positions ----------

#[post("/chassis/spares")]
async fn create_spare(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    body: web::Json<CreateSpareInput>,
) -> ApiResult<HttpResponse> {
    let inp = body.into_inner();
    if inp.spare_index < 1 {
        return Err(ApiError::BadRequest("spare_index must be >= 1".into()));
    }
    let id = inp.id.unwrap_or_else(Uuid::new_v4);
    let row: PositionRow = sqlx::query_as(
        r#"INSERT INTO chassis_positions (
              id, layout_id, axle_id, side, depth, is_spare, spare_index, position_code,
              created_by_user_id, updated_by_user_id
           ) VALUES ($1, $2, NULL, NULL, NULL, TRUE, $3, $4, $5, $5)
           RETURNING *"#,
    )
    .bind(id)
    .bind(inp.layout_id)
    .bind(inp.spare_index)
    .bind(spare_code(inp.spare_index))
    .bind(claims.user_id)
    .fetch_one(pool.get_ref())
    .await?;
    Ok(HttpResponse::Created().json(row))
}

#[derive(serde::Deserialize)]
struct PositionListQuery {
    layout_id: Option<Uuid>,
    axle_id: Option<Uuid>,
}

#[get("/chassis/positions")]
async fn list_positions(
    pool: web::Data<PgPool>,
    q: web::Query<PositionListQuery>,
) -> ApiResult<HttpResponse> {
    let rows: Vec<PositionRow> = match (q.layout_id, q.axle_id) {
        (_, Some(aid)) => {
            sqlx::query_as(
                "SELECT * FROM chassis_positions WHERE deleted_at IS NULL AND axle_id = $1 ORDER BY position_code",
            )
            .bind(aid)
            .fetch_all(pool.get_ref())
            .await?
        }
        (Some(lid), None) => {
            sqlx::query_as(
                "SELECT * FROM chassis_positions WHERE deleted_at IS NULL AND layout_id = $1 ORDER BY position_code",
            )
            .bind(lid)
            .fetch_all(pool.get_ref())
            .await?
        }
        (None, None) => {
            sqlx::query_as(
                "SELECT * FROM chassis_positions WHERE deleted_at IS NULL ORDER BY layout_id, position_code LIMIT 5000",
            )
            .fetch_all(pool.get_ref())
            .await?
        }
    };
    Ok(HttpResponse::Ok().json(rows))
}

#[delete("/chassis/positions/{id}")]
async fn delete_position(
    pool: web::Data<PgPool>,
    claims: AuthClaims,
    path: web::Path<Uuid>,
) -> ApiResult<HttpResponse> {
    let id = path.into_inner();
    // Only spare positions are individually deletable; axle positions are managed via axle.
    let curr: Option<PositionRow> = sqlx::query_as(
        "SELECT * FROM chassis_positions WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool.get_ref())
    .await?;
    let curr = curr.ok_or_else(|| ApiError::NotFound(format!("position {id}")))?;
    if !curr.is_spare {
        return Err(ApiError::BadRequest(
            "non-spare positions cannot be deleted directly; delete the axle instead".into(),
        ));
    }
    sqlx::query(
        r#"UPDATE chassis_positions SET deleted_at = now(), updated_at = now(),
                  updated_by_user_id = $1, sync_version = sync_version + 1
           WHERE id = $2"#,
    )
    .bind(claims.user_id)
    .bind(id)
    .execute(pool.get_ref())
    .await?;
    Ok(HttpResponse::NoContent().finish())
}

// ---------- Full layout for a vehicle ----------

#[get("/chassis/{vehicle_id}/full")]
async fn chassis_full(
    pool: web::Data<PgPool>,
    path: web::Path<i32>,
) -> ApiResult<HttpResponse> {
    let vehicle_id = path.into_inner();

    let layout: Option<LayoutRow> = sqlx::query_as(
        "SELECT * FROM chassis_layouts WHERE vehicle_id = $1 AND deleted_at IS NULL",
    )
    .bind(vehicle_id)
    .fetch_optional(pool.get_ref())
    .await?;
    let Some(layout) = layout else {
        return Err(ApiError::NotFound(format!(
            "no layout for vehicle {vehicle_id}"
        )));
    };

    // Axles ordered (section, section_index)
    let axles: Vec<AxleRow> = sqlx::query_as(
        "SELECT * FROM chassis_axles WHERE layout_id = $1 AND deleted_at IS NULL ORDER BY section, section_index",
    )
    .bind(layout.id)
    .fetch_all(pool.get_ref())
    .await?;

    let positions: Vec<PositionRow> = sqlx::query_as(
        "SELECT * FROM chassis_positions WHERE layout_id = $1 AND deleted_at IS NULL ORDER BY is_spare, position_code",
    )
    .bind(layout.id)
    .fetch_all(pool.get_ref())
    .await?;

    // Tires currently mounted on these positions
    #[derive(sqlx::FromRow, serde::Serialize)]
    struct MountedTire {
        position_id: Uuid,
        id: Uuid,
        dot_code: String,
        brand: String,
        production_year: Option<i16>,
        production_week: Option<i16>,
    }
    let mounted: Vec<MountedTire> = sqlx::query_as(
        r#"
        SELECT ta.position_id,
               t.id, t.dot_code, t.brand, t.production_year, t.production_week
          FROM tire_assignments ta
          JOIN tires t ON t.id = ta.tire_id
         WHERE ta.dismounted_at IS NULL
           AND ta.deleted_at IS NULL
           AND ta.position_id = ANY($1)
        "#,
    )
    .bind(&positions.iter().map(|p| p.id).collect::<Vec<_>>())
    .fetch_all(pool.get_ref())
    .await?;
    use std::collections::HashMap;
    let mut tire_by_pos: HashMap<Uuid, &MountedTire> = HashMap::new();
    for t in &mounted {
        tire_by_pos.insert(t.position_id, t);
    }

    // Group positions by axle
    let mut by_axle: HashMap<Uuid, Vec<&PositionRow>> = HashMap::new();
    let mut spares: Vec<&PositionRow> = Vec::new();
    for p in &positions {
        if p.is_spare {
            spares.push(p);
        } else if let Some(aid) = p.axle_id {
            by_axle.entry(aid).or_default().push(p);
        }
    }
    spares.sort_by_key(|p| p.spare_index);

    let render_position = |p: &PositionRow| {
        let tire = tire_by_pos
            .get(&p.id)
            .map(|t| serde_json::to_value(t).unwrap_or(serde_json::Value::Null));
        json!({
            "id": p.id,
            "position_code": p.position_code,
            "side": p.side,
            "depth": p.depth,
            "is_spare": p.is_spare,
            "spare_index": p.spare_index,
            "tire": tire,
        })
    };

    let mut sections: Vec<serde_json::Value> = Vec::new();
    for section_kind in [
        crate::models::chassis::ChassisSection::Tractor,
        crate::models::chassis::ChassisSection::Trailer,
    ] {
        let axles_for_section: Vec<&AxleRow> =
            axles.iter().filter(|a| a.section == section_kind).collect();
        let axles_json: Vec<_> = axles_for_section
            .iter()
            .map(|a| {
                let pos: Vec<_> = by_axle
                    .get(&a.id)
                    .map(|v| {
                        let mut sorted = v.clone();
                        sorted.sort_by(|x, y| x.position_code.cmp(&y.position_code));
                        sorted.iter().map(|p| render_position(p)).collect()
                    })
                    .unwrap_or_default();
                json!({
                    "id": a.id,
                    "section_index": a.section_index,
                    "axle_type": a.axle_type,
                    "label_ar": a.label_ar,
                    "label_en": a.label_en,
                    "is_steering": a.is_steering,
                    "is_lifted": a.is_lifted,
                    "positions": pos,
                })
            })
            .collect();
        sections.push(json!({
            "section": section_kind,
            "axles": axles_json,
        }));
    }

    let spares_json: Vec<_> = spares.iter().map(|p| render_position(p)).collect();
    Ok(HttpResponse::Ok().json(json!({
        "layout": {
            "id": layout.id,
            "vehicle_id": layout.vehicle_id,
            "name_ar": layout.name_ar,
            "name_en": layout.name_en,
        },
        "sections": sections,
        "spares": spares_json,
    })))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(create_layout)
        .service(list_layouts)
        .service(get_layout)
        .service(update_layout)
        .service(delete_layout)
        .service(create_axle)
        .service(list_axles)
        .service(update_axle)
        .service(delete_axle)
        .service(create_spare)
        .service(list_positions)
        .service(delete_position)
        .service(chassis_full);
}
