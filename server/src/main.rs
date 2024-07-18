use actix_web::{error, get, post, web, App, HttpResponse, HttpServer, Responder};
use anyhow::Result;
use clap::Parser;
use image_collection::{ImageCollection, ImageCollectionOptions, Match};

#[derive(Parser, Debug)]
#[clap(about, version, author)]
struct Args {
    /// The output database. Must be a file as only sqlite is currently supported
    /// Write the path in the form of "sqlite://<rel-path>"
    #[clap(short, long, default_value = "sqlite://out.db")]
    output: String,

    /// The queue buffer. Candidates gets pre-computed.
    /// The precomputation lowers the precision of the matchmaking
    /// but also reduces the possible latency of the next match up
    #[clap(long, default_value_t = 20)]
    queue_buffer: usize,

    /// The port the server will listen to
    #[clap(long, default_value_t = 8000)]
    port: usize,

    /// directory of the image
    #[clap(long, default_value = "./images")]
    image_dir: String,
}

#[get("/")]
async fn index() -> impl Responder {
    actix_files::NamedFile::open("static/index.html")
}

#[get("/style.css")]
async fn style() -> impl Responder {
    actix_files::NamedFile::open_async("static/picnic.min.css").await
}

#[get("/matches")]
async fn return_new_match(
    collection: web::Data<ImageCollection>,
) -> actix_web::Result<HttpResponse> {
    match collection.new_duel().await {
        Ok(new_duel) => Ok(HttpResponse::Ok().json(new_duel)),
        Err(err) => Err(error::ErrorBadRequest(err.to_string())),
    }
}

#[post("/matches")]
async fn on_new_score(
    m: actix_web::web::Json<Match>,
    collection: web::Data<ImageCollection>,
) -> actix_web::Result<HttpResponse> {
    collection.insert_match(m.to_owned()).await;
    match collection.new_duel().await {
        Ok(new_duel) => Ok(HttpResponse::Ok().json(new_duel)),
        Err(err) => Err(error::ErrorBadRequest(err.to_string())),
    }
}

#[actix_web::main]
async fn main() -> Result<()> {
    env_logger::builder()
        //.filter_level(log::LevelFilter::Info)
        .init();

    let args = Args::parse();

    let options = ImageCollectionOptions {
        db_path: args.output,
        candidate_buffer: args.queue_buffer,
    };
    let img_col = ImageCollection::new(&options, &args.image_dir).await?;

    let addr = format!("[::]:{}", args.port);
    let image_dir = args.image_dir;
    let img_col_closure = img_col.clone();
    let server = HttpServer::new(move || {
        App::new()
            .app_data(actix_web::web::Data::new(img_col_closure.clone()))
            .wrap(actix_web::middleware::Logger::new("%r - %s - %Dms"))
            .service(index)
            .service(return_new_match)
            .service(on_new_score)
            .service(actix_files::Files::new("/images", &image_dir))
            .service(style)
    })
    .keep_alive(std::time::Duration::new(90, 0))
    .bind(&addr)?;

    println!("Start Server on {}.", &addr);
    server.run().await?;
    img_col.close().await;
    Ok(())
}
