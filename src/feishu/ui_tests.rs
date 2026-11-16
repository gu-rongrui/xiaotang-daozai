use super::{H,ENV_LOCK};
use crate::{feishu::*,app::{Env,TODO_PAGE},paint::Card};
use eframe::egui::{self,Pos2,Rect};
use serde_json::json;

fn setup()->H {
    let mut h=H::new();h.app.settings.pet=false;
    h.app.feishu.store=Store{version:1,account:"me".into(),profile:"app".into(),config:Config{enabled:true,writes:true,selected:vec!["cal".into()],..Default::default()},
        calendars:vec![json!({"calendar_id":"cal","summary":"个人安排","role":"owner","type":"primary","color":0x41b9a0})],
        tasks:vec![json!({"guid":"one","summary":"整理今天的实验记录","members":[{"id":"me","role":"assignee"}],"completed_at":"0"}),json!({"guid":"two","summary":"阅读一篇文献","members":[{"id":"me","role":"assignee"}],"completed_at":"123"})],
        events:vec![SavedEvent{calendar:"cal".into(),raw:json!({"event_id":"example_0","organizer_calendar_id":"cal","summary":"整理实验数据","start_time":{"date":"2026-10-28"},"end_time":{"date":"2026-10-29"},"color":0})}],..Default::default()};
    h.app.feishu.events=h.app.feishu.store.view_events();h.app.feishu.status="已同步 · 09:41".into();h
}

#[test] fn api_calendar_keeps_local_data_and_suppresses_legacy_feishu(){
    let _g=ENV_LOCK.lock().unwrap_or_else(|e|e.into_inner());let mut h=setup();
    h.app.calendar.config=Some(crate::calendar::Config{provider:crate::calendar::Provider::Feishu,username:"example".into(),href:None,server:None});
    assert!(h.app.calendar_configs().is_empty());
    let day=chrono::NaiveDate::from_ymd_opt(2026,10,28).unwrap();
    let entries=h.app.calendar_entries(day);assert_eq!(entries.len(),1);
    assert!(entries[0].sources.iter().any(|s|matches!(s,crate::calendar_ui::Source::Feishu(_,"个人安排",_))));
    h.app.feishu.store.config.enabled=false;assert_eq!(h.app.calendar_configs().len(),1);
    assert!(h.app.calendar_entries(day).is_empty());
}

#[test] fn cloud_calendar_editor_uses_cloud_identity_and_disables_delete(){
    let _g=ENV_LOCK.lock().unwrap_or_else(|e|e.into_inner());let mut h=setup();
    let uid=h.app.feishu.events[0].meeting.uid.clone();h.app.open_calendar_editor(Some(&uid));
    assert_eq!(h.app.feishu.calendar_event,"example_0");assert_eq!(h.app.feishu.calendar_target,"cal");
    assert!(h.app.calendar.local.is_empty());
}

#[test] fn cloud_task_editor_opens_in_original_panel(){
    let _g=ENV_LOCK.lock().unwrap_or_else(|e|e.into_inner());let mut h=setup();
    h.app.show_page(TODO_PAGE);h.hover(H::local(250.0,70.0),1.0);h.click(H::local(79.0,109.0));
    assert!(h.app.feishu.editing.is_some());assert!(h.app.todos.items.is_empty());
}

#[test]
#[ignore="Export mock-data previews on request"]
fn personal_ui_gallery(){
    let _g=ENV_LOCK.lock().unwrap_or_else(|e|e.into_inner());let mut h=setup();
    let dest=std::path::PathBuf::from(std::env::var_os("DAOZAI_FEISHU_PREVIEW_DIR").unwrap());std::fs::create_dir_all(&dest).unwrap();
    h.app.calendar.view.day=chrono::NaiveDate::from_ymd_opt(2026,10,28).unwrap();h.app.calendar.view.month=chrono::NaiveDate::from_ymd_opt(2026,10,1).unwrap();
    let env=Env{today:"2026-10-28".into(),clock:"09:41:00".into(),..Default::default()};
    let mut raster=crate::preview_raster::Raster::new(419,216);
    for scene in ["todo","calendar","calendar-editor","settings"]{
        if scene=="calendar-editor"{h.app.open_calendar_editor(None);}
        let (width,height)=if scene=="settings"{(620,790)}else{(419,216)};
        raster.w=width;raster.h=height;raster.buf.resize(width*height,[0.0;4]);
        for frame in 0..4 {
            let rect=Rect::from_min_size(Pos2::ZERO,egui::vec2(width as f32,height as f32));
            let out=h.ctx.run(egui::RawInput{screen_rect:Some(rect),..Default::default()},|ctx|{
                egui::CentralPanel::default().frame(egui::Frame::NONE.fill(egui::Color32::BLACK)).show(ctx,|ui|{
                    if scene=="settings"{h.app.feishu_settings(ui);}else{
                        let c=Card::new(ui,Pos2::ZERO,rect);
                        match scene{"todo"=>h.app.page_feishu_todo(&c,&env),"calendar"=>h.app.page_calendar(&c,&env),_=>h.app.draw_calendar_editor(&c)}
                    }
                });
            });
            raster.update_textures(&out.textures_delta);
            if frame==3 {
                raster.buf.fill([0.0,0.0,0.0,1.0]);raster.draw(&h.ctx.tessellate(out.shapes,out.pixels_per_point),out.pixels_per_point);
                let mut rgba=Vec::new();raster.to_rgba8(&mut rgba);image::save_buffer(dest.join(format!("{scene}.png")),&rgba,width as u32,height as u32,image::ColorType::Rgba8).unwrap();
            }
            raster.free_textures(&out.textures_delta);
        }
    }
}
