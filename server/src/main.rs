use actix_web::{App, HttpResponse, HttpServer, Responder, error, get, post, web};
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
            .service(actix_files::Files::new("/images", &image_dir).prefer_utf8(true))
            .service(style)
    })
    .keep_alive(std::time::Duration::new(90, 0))
    .tcp_nodelay(true)
    .bind(&addr)?;

    println!("Start Server on {}.", &addr);
    server.run().await?;
    img_col.close().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{test, web, App};
    use image_collection::MatchOutcome;

    #[actix_web::test]
    async fn test_get_matches_returns_duel() {
        let img_col = ImageCollection::new_pre_configured(5).await.unwrap();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(img_col))
                .service(return_new_match),
        )
        .await;

        let req = test::TestRequest::get().uri("/matches").to_request();
        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        let duel: image_collection::Duel = test::read_body_json(resp).await;
        assert_ne!(duel.home_id, duel.guest_id);
        assert!(!duel.home.is_empty());
        assert!(!duel.guest.is_empty());
    }

    #[actix_web::test]
    async fn test_post_matches_accepts_valid_match_and_returns_duel() {
        let img_col = ImageCollection::new_pre_configured(5).await.unwrap();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(img_col))
                .service(return_new_match)
                .service(on_new_score),
        )
        .await;

        let m = Match {
            home_id: 1,
            guest_id: 2,
            won: MatchOutcome::HomeWin,
        };
        let req = test::TestRequest::post()
            .uri("/matches")
            .set_json(&m)
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        let duel: image_collection::Duel = test::read_body_json(resp).await;
        assert_ne!(duel.home_id, duel.guest_id);
    }

    #[actix_web::test]
    async fn test_post_matches_invalid_json_returns_400() {
        let img_col = ImageCollection::new_pre_configured(5).await.unwrap();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(img_col))
                .service(on_new_score),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/matches")
            .insert_header(("Content-Type", "application/json"))
            .set_payload("not valid json")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 400);
    }

    #[actix_web::test]
    async fn test_post_matches_invalid_outcome_returns_400() {
        let img_col = ImageCollection::new_pre_configured(5).await.unwrap();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(img_col))
                .service(on_new_score),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/matches")
            .insert_header(("Content-Type", "application/json"))
            .set_payload(r#"{"home_id": 1, "guest_id": 2, "won": 2.0}"#)
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 400);
    }

    #[actix_web::test]
    async fn test_get_matches_no_players_returns_error() {
        let img_col = ImageCollection::new_pre_configured(0).await.unwrap();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(img_col))
                .service(return_new_match),
        )
        .await;

        let req = test::TestRequest::get().uri("/matches").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 400);
    }
}
