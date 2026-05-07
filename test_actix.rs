use actix_web::{get, web, App, HttpServer, HttpResponse, Responder};

#[get("/test")]
async fn test() -> impl Responder {
    HttpResponse::Ok().body("test ok")
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    HttpServer::new(|| {
        App::new()
            .service(
                web::scope("/api")
                    .service(
                        web::scope("")
                            .service(test)
                    )
            )
    })
    .bind(("127.0.0.1", 9998))?
    .run()
    .await
}
