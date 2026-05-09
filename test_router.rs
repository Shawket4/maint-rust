use actix_web::{get, web, App, HttpServer, HttpResponse, Responder};

#[get("/cache/vehicles")]
async fn get_cache_vehicles() -> impl Responder {
    HttpResponse::Ok().body("vehicles ok")
}

#[get("/health")]
async fn health() -> impl Responder {
    HttpResponse::Ok().body("health ok")
}


#[actix_web::main]
async fn main() -> std::io::Result<()> {
    HttpServer::new(|| {
        App::new()
            .service(
                web::scope("/api/maint")
                    .service(web::scope("").service(health))
                    .service(
                        web::scope("")
                            .service(
                                web::scope("").service(get_cache_vehicles)
                            )
                    )
            )
    })
    .bind(("127.0.0.1", 9999))?
    .run()
    .await
}
