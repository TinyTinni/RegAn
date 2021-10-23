#[macro_use]
extern crate log;

mod image_collection;

use actix_web::{error, get, post, web, App, HttpResponse, HttpServer, Responder};
use anyhow::Result;
use image_collection::{ImageCollection, Match, ImageCollectionOptions};

#[get("/")]
async fn index() -> impl Responder {
    actix_files::NamedFile::open("static/index.html")
}

#[get("/matches")]
async fn return_new_match(
    collection: web::Data<ImageCollection>,
) -> actix_web::Result<HttpResponse> {
    let now = std::time::Instant::now();
    match collection.get_ref().new_duel().await {
        Ok(new_duel) => {
            let payload = HttpResponse::Ok().json(new_duel);
            info!("get matches: {} microseconds", now.elapsed().as_micros());
            return Ok(payload);
        }
        Err(err) => {
            info!("get matches: {} microseconds", now.elapsed().as_micros());
            return Err(error::ErrorBadRequest(err.to_string()));
        }
    }
}

#[post("/played_match")]
async fn on_new_score(
    m: actix_web::web::Json<Match>,
    collection: web::Data<ImageCollection>,
) -> actix_web::Result<HttpResponse> {
    let now = std::time::Instant::now();
    match collection.get_ref().insert_match(&m).await {
        Ok(new_duel) => {
            let payload = HttpResponse::Ok().json(new_duel);
            info!("post scores: {} microseconds", now.elapsed().as_micros());
            return Ok(payload);
        }
        Err(err) => {
            info!("get matches: {} microseconds", now.elapsed().as_micros());
            return Err(error::ErrorBadRequest(err.to_string()));
        }
    }
}

#[actix_web::main]
async fn main() -> Result<()> {
    env_logger::builder()
        //.filter_level(log::LevelFilter::Info)
        .init();

    let options = ImageCollectionOptions {
        db_path: "sqlite://:memory:".to_owned(),
        candidate_buffer: 20,
    };
    let img_col = ImageCollection::new(&options).await?;

    let addr = "127.0.0.1:8000";
    let server = HttpServer::new(move || {
        App::new()
            .app_data(actix_web::web::Data::new(img_col.clone()))
            .service(index)
            .service(return_new_match)
            .service(on_new_score)
            .service(actix_files::Files::new("/images", "./images"))
    })
    .keep_alive(90)
    .bind(addr)?;

    println!("Start Server on {}.", addr);
    server.run().await?;
    Ok(())
}
