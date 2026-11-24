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

#[test]
fn cloud_task_due_times_render_without_crowding_the_header() {
    use chrono::TimeZone;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let mut h = setup();
    let due = chrono::Local.with_ymd_and_hms(2026, 11, 28, 9, 0, 0).single().unwrap();
    h.app.feishu.status = "已同步 · 2026-11-24 15:25".into();
    h.app.feishu.store.tasks[0]["due"] = json!({
        "timestamp": due.timestamp_millis().to_string(), "is_all_day": false
    });
    h.app.feishu.store.tasks.push(json!({
        "guid": "all-day", "summary": "仅有日期的任务", "completed_at": "0",
        "due": { "timestamp": due.timestamp_millis(), "is_all_day": true }
    }));

    h.app.show_page(TODO_PAGE);
    h.hover(H::local(250.0, 70.0), 1.0);
    let labels: Vec<_> = h.shapes.iter().filter_map(|shape| match &shape.shape {
        egui::Shape::Text(text) => Some(text.galley.text()),
        _ => None,
    }).collect();
    assert!(labels.contains(&"10月28日 周三"), "keep the center date");
    assert!(labels.contains(&"11/28 09:00"));
    assert!(labels.contains(&"11/28"));
    assert!(labels.contains(&"阅读一篇文献"), "keep the task without a deadline");
    assert!(!labels.iter().any(|label| label.contains("15:25")));
    assert!(!labels.contains(&"编辑"));
    assert!(!labels.contains(&"只读"));
}

#[test] fn cloud_task_editor_opens_in_original_panel(){
    let _g=ENV_LOCK.lock().unwrap_or_else(|e|e.into_inner());let mut h=setup();
    h.app.show_page(TODO_PAGE);h.hover(H::local(250.0,70.0),1.0);h.click(H::local(79.0,109.0));
    assert!(h.app.feishu.editing.is_some());assert!(h.app.todos.items.is_empty());
    assert!(h.app.is_editing(),"Cloud text entry must enable the native keyboard focus mode");
    assert!(h.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::Text(t)if t.galley.text().contains("要做点什么？回车保存"))));
    h.frame(vec![egui::Event::Text("输入测试".into())]);
    assert!(h.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::Text(t)if t.galley.text().contains("输入测试"))));
    assert!(!h.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::Text(t)if t.galley.text().contains("截止："))));
    h.frame(vec![egui::Event::Key{key:egui::Key::Enter,physical_key:None,pressed:true,repeat:false,modifiers:Default::default()}]);
    assert!(h.app.feishu.submitted.is_some(),"Enter must save the single-line cloud task");
    h.app.cancel_edit();assert!(h.app.feishu.editing.is_none());
}

#[test] fn cloud_task_ime_confirmation_does_not_submit(){
    let _g=ENV_LOCK.lock().unwrap_or_else(|e|e.into_inner());let mut h=setup();
    h.app.show_page(TODO_PAGE);h.hover(H::local(250.0,70.0),1.0);h.click(H::local(79.0,109.0));
    h.frame(vec![egui::Event::Ime(egui::ImeEvent::Enabled)]);
    let enter=||egui::Event::Key{key:egui::Key::Enter,physical_key:None,pressed:true,repeat:false,modifiers:Default::default()};
    h.frame(vec![enter(),egui::Event::Ime(egui::ImeEvent::Commit("中文待办".into())),egui::Event::Ime(egui::ImeEvent::Disabled)]);
    assert!(h.app.feishu.submitted.is_none(),"IME confirmation must only commit the Chinese text");
    h.frame(vec![enter()]);assert!(h.app.feishu.submitted.is_some());
}

#[test] fn queued_calendar_creation_stays_visible_without_a_cloud_response(){
    let _g=ENV_LOCK.lock().unwrap_or_else(|e|e.into_inner());let mut h=setup();
    h.app.feishu.store.queue.push(Change::new("event-create","","cal",serde_json::Value::Null,json!({"summary":"离线新日程","start_time":{"date":"2026-10-29"},"end_time":{"date":"2026-10-30"}})));
    let events=h.app.feishu.store.view_events();assert_eq!(events.len(),2);
    let draft=events.iter().find(|e|e.meeting.title=="离线新日程").unwrap();assert!(draft.name.ends_with("待同步"));
    assert!(!event_writable(&draft.raw,&draft.calendar));
}

#[test]
#[ignore="Export mock-data previews on request"]
fn personal_ui_gallery(){
    let _g=ENV_LOCK.lock().unwrap_or_else(|e|e.into_inner());let mut h=setup();
    let dest=std::path::PathBuf::from(std::env::var_os("DAOZAI_FEISHU_PREVIEW_DIR").unwrap());std::fs::create_dir_all(&dest).unwrap();
    h.app.calendar.view.day=chrono::NaiveDate::from_ymd_opt(2026,10,28).unwrap();h.app.calendar.view.month=chrono::NaiveDate::from_ymd_opt(2026,10,1).unwrap();
    let env=Env{today:"2026-10-28".into(),clock:"09:41:00".into(),..Default::default()};
    let mut raster=crate::preview_raster::Raster::new(419,216);
    for scene in ["todo","calendar","calendar-editor","settings","todo-input"]{
        if scene=="todo-input"{
            h.app.cancel_edit();h.app.show_page(TODO_PAGE);h.hover(H::local(250.0,70.0),1.0);h.click(H::local(79.0,109.0));
            // A fresh context lets the raster receive every glyph upload for this scene.
            let mut fresh=setup();fresh.app.feishu.editing=h.app.feishu.editing.take();h=fresh;
            raster=crate::preview_raster::Raster::new(419,216);
        }
        if scene=="calendar-editor"{h.app.open_calendar_editor(None);}
        let (width,height)=if scene=="settings"{(620,790)}else{(419,216)};
        raster.w=width;raster.h=height;raster.buf.resize(width*height,[0.0;4]);
        for frame in 0..4 {
            let rect=Rect::from_min_size(Pos2::ZERO,egui::vec2(width as f32,height as f32));
            let out=h.ctx.run(egui::RawInput{screen_rect:Some(rect),..Default::default()},|ctx|{
                egui::CentralPanel::default().frame(egui::Frame::NONE.fill(egui::Color32::BLACK)).show(ctx,|ui|{
                    if scene=="settings"{h.app.feishu_settings(ui);}else{
                        let c=Card::new(ui,Pos2::ZERO,rect);
                        match scene{"todo"|"todo-input"=>h.app.page_feishu_todo(&c,&env),"calendar"=>h.app.page_calendar(&c,&env),_=>h.app.draw_calendar_editor(&c)}
                    }
                    if scene=="todo-input"{h.app.feishu_task_inputs(ui,Pos2::ZERO,&mut vec![]);}
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
