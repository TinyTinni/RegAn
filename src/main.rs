#[macro_use]
extern crate log;

mod image_collection;

use actix_files::NamedFile;
use actix_web::{error, get, post, web, App, HttpRequest, HttpResponse, HttpServer, Responder};
use anyhow::Result;
use sqlx::SqlitePool;
use image_collection::{calculate_new_match, check_db_integrity, insert_match, update_rating, Match};

#[get("/")]
async fn index() -> impl Responder {
    NamedFile::open("static/index.html")
}

#[get("/images/{filename:.*}")]
async fn return_image(req: HttpRequest) -> impl Responder {
    let path = req.match_info().query("filename");
    NamedFile::open(format!("images/{}", path))
}

#[get("/matches")]
async fn return_new_match(db_pool: web::Data<SqlitePool>) -> actix_web::Result<HttpResponse> {
    match calculate_new_match(&db_pool.get_ref()).await {
        Ok(new_duel) => return Ok(HttpResponse::Ok().json(new_duel)),
        Err(err) => return Err(error::ErrorBadRequest(err.to_string())),
    }
}

#[post("/scores")]
async fn on_new_score(
    m: actix_web::web::Json<Match>,
    db_pool: web::Data<SqlitePool>,
) -> actix_web::Result<HttpResponse> {

    let now = std::time::Instant::now();
    let insert_and_create = async {
        let db = db_pool.get_ref();
        insert_match(&db, &m).await?;
        update_rating(&db, &m).await?;
        calculate_new_match(&db).await
    }.await;
    let time_ealpsed = std::time::Instant::now() - now;
    info!("Request computation took {}ms", time_ealpsed.as_millis());

    match insert_and_create {
        Ok(new_duel) => return Ok(HttpResponse::Ok().json(new_duel)),
        Err(err) => return Err(error::ErrorBadRequest(err.to_string())),
    }
}

#[actix_web::main]
async fn main() -> Result<()> {
    env_logger::builder()
        //.filter_level(log::LevelFilter::Info)
        .init();

    let db_name = "sqlite://test.db";

    info!("Connect to database: {}", db_name);
    let db_pool = SqlitePool::connect(&db_name).await?;
    sqlx::query_file!("./schema.sql").execute(&db_pool).await?;
    check_db_integrity(&db_pool).await?;

    let addr = "127.0.0.1:8000";
    let server = HttpServer::new(move || {
        App::new()
            //.wrap(actix_web::middleware::Compress::default())
            .app_data(actix_web::web::Data::new(db_pool.clone()))
            .service(index)
            .service(return_new_match)
            .service(return_image)
            .service(on_new_score)
    })
    .keep_alive(90)
    .bind(addr)?;

    info!("Start Server on {}.", addr);
    server.run().await?;
    Ok(())
}
