
use actix_identity::{CookieIdentityPolicy, IdentityService, Identity};
use actix_web_actors::ws;
use chrono::Utc;
use hmac::{Hmac, NewMac};
use jwt::{SignWithKey};
use serde_json::json;
use crate::{Result, dvr::DVR, database::Database, model::{DetectionFilter, EventFilter, RangeFilter, CameraPath, Alert, AlertPath, EventPath, UserAuth, UserCredentials, WSToken, CreateUser, JumpFilter, ExportFilter}};

use sha2::Sha256;

mod auth;
mod error;
use error::ServiceError;

use crate::model::{SourceConfig, Region};

use actix_web::{get, post, delete, web::{Query, Json, Path}, App, HttpResponse, HttpServer, Responder, web::{Data, self}, middleware, cookie::time::Duration, HttpRequest, http};
use actix_files::Files;

#[get("/api/event")]
async fn get_events(db: Data<Database>, _user: UserAuth, filter: Query<EventFilter>) -> impl Responder {

    if let Ok(d) = db.list_events(filter.0.start).await {
        Some(Json(d))
    } else {
        None
    }
}

#[get("/api/event/{id}")]
async fn get_event(db: Data<Database>, _user: UserAuth, id: Path<EventPath>) -> impl Responder {
    if let Ok(d) = db.get_event(id.into_inner().id).await {
        Some(Json(d))
    } else {
        None
    }
}

#[get("/api/event/range")]
async fn get_event_range(db: Data<Database>, _user: UserAuth, filter: Query<RangeFilter>) -> impl Responder {
    let f = filter.into_inner();
    if let Ok(d) = db.list_events_range(f.source, f.from, f.to).await {
        Some(Json(d))
    } else {
        None
    }
}

#[get("/api/event/previous")]
async fn get_event_previous(db: Data<Database>, _user: UserAuth, filter: Query<JumpFilter>) -> impl Responder {
    let f = filter.into_inner();
    if let Ok(d) = db.list_event_previous(f.source, f.from).await {
        Some(Json(d))
    } else {
        None
    }
}

#[get("/api/event/next")]
async fn get_event_next(db: Data<Database>, _user: UserAuth, filter: Query<JumpFilter>) -> impl Responder {
    let f = filter.into_inner();
    if let Ok(d) = db.list_event_next(f.source, f.from).await {
        Some(Json(d))
    } else {
        None
    }
}

#[get("/api/cameras")]
async fn get_cameras(db: Data<Database>, _user: UserAuth) -> impl Responder {
    if let Ok(d) = db.list_cameras().await {
        Some(Json(d))
    } else {
        None
    }
}

#[get("/api/cameras/{id}")]
async fn get_camera(db: Data<Database>, _user: UserAuth, id: Path<CameraPath>) -> impl Responder {
    let p = id.into_inner();
    
    if let Ok(d) = db.get_camera(p.id).await {
        if let Some(d) = d {
            Some(Json(d))
        } else {
            None
        }
    } else {
        None
    }
}

#[get("/api/cameras/{id}/export")]
async fn export(_user: UserAuth, dvr: Data<DVR>, id: Path<CameraPath>, filter: Query<ExportFilter>) -> Result<HttpResponse, ServiceError> {
    let dvr = dvr.into_inner();
    let filter = filter.into_inner();
    let id = id.into_inner();
    let data = crate::client::export(id.id, filter.from, filter.to, &*dvr).await?;
    
    Ok(
        HttpResponse::Ok()
            .content_type("video/mp4")
            .insert_header((http::header::CONTENT_DISPOSITION, format!("attachment; filename=\"{}.mp4\"", filter.from.as_millis())))
            .body(data)
    )
}

#[post("/api/cameras/{id}")]
async fn update_camera(db: Data<Database>, dvr: Data<DVR>, _user: UserAuth, id: Path<CameraPath>, camera: Json<SourceConfig>) -> impl Responder {
    let p = id.into_inner();
    let mut camera = camera.into_inner();
    camera._id = p.id;
    
    if let Ok(_) = db.update_camera(&camera).await {
        dvr.remove_camera(&camera._id).await;
        let _ = dvr.update_camera(&camera).await;
        Some(Json(camera))
    } else {
        None
    }
}

#[get("/api/cameras/{id}/regions")]
async fn get_regions(db: Data<Database>, _user: UserAuth, id: Path<CameraPath>) -> impl Responder {
    let p = id.into_inner();
    
    if let Ok(d) = db.list_regions_camera(p.id).await {
        Some(Json(d))
    } else {
        None
    }
}

#[post("/api/cameras/{id}/regions")]
async fn update_regions(db: Data<Database>, dvr: Data<DVR>, _user: UserAuth, id: Path<CameraPath>, regions: Json<Vec<Region>>) -> impl Responder {
    let p = id.into_inner();
    let mut regions = regions.into_inner();
    
    if let Ok(_) = db.update_regions(p.id, &mut regions).await {
        let _ = dvr.update_regions(p.id, &regions).await;
        Some(Json(regions))
    } else {
        None
    }
}

#[get("/api/cameras/{id}/image.jpg")]
async fn get_camera_image(dvr: Data<DVR>, _user: UserAuth, id: Path<CameraPath>) -> impl Responder {
    let p = id.into_inner();
    
    if let Ok(d) = dvr.camera_image(p.id).await {
        HttpResponse::Ok()
            .content_type("image/jpeg")
            .body(d.to_vec())
            
    } else {
        HttpResponse::NotFound()
            .body("")
    }
}

#[get("/api/detections")]
async fn get_detections(db: Data<Database>, _user: UserAuth, filter: Query<DetectionFilter>) -> impl Responder {
    let f = filter.into_inner();
    if let Ok(d) = db.get_detections(f.source, f.at).await {
        Some(Json(d))
    } else {
        None
    }
}

#[get("/api/alerts")]
async fn get_alerts(db: Data<Database>, _user: UserAuth) -> impl Responder {
    if let Ok(d) = db.list_alerts().await {
        Some(Json(d))
    } else {
        None
    }
}

#[post("/api/alerts")]
async fn add_alert(db: Data<Database>, dvr: Data<DVR>, _user: UserAuth, alert: Json<Alert>) -> impl Responder {
    let alert = alert.into_inner();
    if let Ok(_) = db.add_alert(&alert).await {
        dvr.add_alert(alert.clone()).await;
        Some(Json(alert))
    } else {
        None
    }
}

#[post("/api/alerts/{id}")]
async fn update_alert(db: Data<Database>, dvr: Data<DVR>, _user: UserAuth, id: Path<AlertPath>, alert: Json<Alert>) -> impl Responder {
    let mut alert = alert.into_inner();
    let p = id.into_inner();
    alert._id = p.id;

    if let Ok(_) = db.update_alert(&alert).await {
        dvr.remove_alert(p.id).await;
        dvr.add_alert(alert.clone()).await;
        Some(Json(alert))
    } else {
        None
    }
}

#[delete("/api/alerts/{id}")]
async fn delete_alert(db: Data<Database>, dvr: Data<DVR>, _user: UserAuth, id: Path<AlertPath>) -> impl Responder {
    let p = id.into_inner();

    if let Ok(_) = db.delete_alert(&p.id).await {
        dvr.remove_alert(p.id).await;
        Some(Json(json!({"ok": true})))
    } else {
        None
    }
}

#[get("/api/auth")]
async fn whoami(mut user: UserAuth, ident: Identity, key: Data<Hmac<Sha256>>) -> impl Responder {
    let key = key.into_inner();
    user.expires = Utc::now() + chrono::Duration::days(14);
    let token = user.clone().sign_with_key(&*key).unwrap();
    ident.remember(token);
    Json(user)
}

#[post("/api/auth_ws")]
async fn auth_ws(mut user: UserAuth, ident: Identity, key: Data<Hmac<Sha256>>) -> impl Responder {
    let key = key.into_inner();
    user.expires = Utc::now() + chrono::Duration::days(14);
    let token = user.clone().sign_with_key(&*key).unwrap();
    ident.remember(token);

    let token = WSToken {
        username: user.username,
        expires: Utc::now() + chrono::Duration::minutes(5),
        scope: "websocket".to_owned()
    };
    let token = token.sign_with_key(&*key).unwrap();
    token
}

#[post("/api/auth")]
async fn login(db: Data<Database>, ident: Identity, key: Data<Hmac<Sha256>>, credentials: Json<UserCredentials>) -> Result<HttpResponse, ServiceError> {
    let key = key.into_inner();
    let credentials = credentials.into_inner();

    if let Ok(result) = db.login(credentials.username, credentials.password).await {
        if let Some(user) = result {
            let token = user.sign_with_key(&*key).unwrap();
            ident.remember(token);
            Ok(HttpResponse::Ok().finish())
        }
        else {
            Err(ServiceError::Unauthorized)
        }
    } else {
        Err(ServiceError::Unauthorized)
    }
}

#[delete("/api/auth")]
async fn logout(ident: Identity) -> Result<HttpResponse, actix_web::Error> {
    ident.forget();
    Ok(HttpResponse::Ok().finish())
}

#[get("/ws")]
async fn ws_index(r: HttpRequest, stream: web::Payload, dvr: Data<DVR>, _user: WSToken) -> Result<HttpResponse, actix_web::Error> {
    ws::start(crate::client::ClientHandler{
        client: None,
        dvr: dvr.downgrade()
    }, &r, stream)
}

#[post("/api/users")]
async fn add_user(db: Data<Database>, user: UserAuth, create_user: Json<CreateUser>) -> Result<HttpResponse, ServiceError> {
    if !user.admin {
        return Err(ServiceError::Unauthorized)
    }
    let _ = db.create_user(create_user.username.clone(), create_user.password.clone(), false).await;

    Ok(HttpResponse::Ok().finish())
}

pub async fn start_server(database: crate::database::Database, dvr: crate::dvr::DVR) -> Result<()> {
    let database = Data::new(database);
    let dvr = Data::new(dvr);
    let key = Data::new(Hmac::<Sha256>::new_from_slice(dvr.config.token_key.as_bytes())?);

    HttpServer::new(move || 
            App::new()
                .wrap(middleware::Compress::default())
                .wrap(IdentityService::new(
                    CookieIdentityPolicy::new(dvr.config.cookie_key.as_bytes())
                        .name("auth")
                        .path("/api/")
                        //.domain(domain.as_str())
                        .max_age(Duration::days(14))
                        .secure(true), // this can only be true if you have https
                ))
                .service(get_events)
                .service(get_event_range)
                .service(get_event_previous)
                .service(get_event_next)
                .service(get_event)
                .service(get_cameras)
                .service(get_camera)
                .service(export)
                .service(update_camera)
                .service(get_regions)
                .service(update_regions)
                .service(get_camera_image)
                .service(get_detections)
                .service(get_alerts)
                .service(add_alert)
                .service(update_alert)
                .service(delete_alert)
                .service(auth_ws)
                .service(ws_index)
                .service(Files::new("/api/event/thumb/", &dvr.config.storage.snapshots))
                
                .service(whoami)
                .service(login)
                .service(logout)
                .service(add_user)
                .app_data(Data::clone(&database))
                .app_data(Data::clone(&dvr))
                .app_data(Data::clone(&key))
        )
        .bind(("0.0.0.0", 8099))?
        .run()
        .await?;
    
    Ok(())
}
