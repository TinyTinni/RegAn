#[macro_use]
extern crate log;

mod image_collection;

use actix_files::NamedFile;
use actix_web::{error, get, post, web, App, HttpRequest, HttpResponse, HttpServer, Responder};
use anyhow::Result;
use image_collection::{ImageCollection, Match, Options};

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
async fn return_new_match(
    collection: web::Data<ImageCollection>,
) -> actix_web::Result<HttpResponse> {
    match collection.get_ref().new_duel().await {
        Ok(new_duel) => return Ok(HttpResponse::Ok().json(new_duel)),
        Err(err) => return Err(error::ErrorBadRequest(err.to_string())),
    }
}

#[post("/scores")]
async fn on_new_score(
    m: actix_web::web::Json<Match>,
    collection: web::Data<ImageCollection>,
) -> actix_web::Result<HttpResponse> {
    match collection.get_ref().insert_match(&m).await {
        Ok(new_duel) => return Ok(HttpResponse::Ok().json(new_duel)),
        Err(err) => return Err(error::ErrorBadRequest(err.to_string())),
    }
}

#[actix_web::main]
async fn main() -> Result<()> {
    env_logger::builder()
        //.filter_level(log::LevelFilter::Info)
        .init();

    let options = Options {
        db_path: "sqlite://test.db".to_owned(),
        candidate_buffer: 20,
    };
    let img_col = ImageCollection::new(&options).await?;

    let addr = "127.0.0.1:8000";
    let server = HttpServer::new(move || {
        App::new()
            .app_data(actix_web::web::Data::new(img_col.clone()))
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
