use super::*;

/// 旧版「新邮件提醒」关着的用户：升级后邮箱模块关闭，内部开关恢复为开，不会突然开始收信。
#[test]
fn merged_mail_switch_migrates_old_off_to_module_off(){
    let _g=ENV_LOCK.lock().unwrap_or_else(|e|e.into_inner());let mut h=H::new();h.frame(vec![]);
    h.app.settings.visible_pages[crate::app::MAIL_PAGE]=true;h.app.settings.mail_enabled=false;
    assert!(!h.app.mail_active());
    h.app.migrate_merged_switches();
    assert!(!h.app.settings.visible_pages[crate::app::MAIL_PAGE],"旧的新邮件提醒关闭，迁移为邮箱模块关闭");
    assert!(h.app.settings.mail_enabled,"内部开关恢复为开，以后只由模块开关控制");
    assert!(!h.app.mail_active(),"迁移前后都不收信");
    assert!(crate::model::Settings::load().mail_enabled,"迁移结果要保存");
    h.app.set_page_visible(crate::app::MAIL_PAGE,true);
    assert!(h.app.mail_active(),"重新打开模块即恢复收信，不再需要第二个开关");
}

#[test]
fn merged_quota_switch_migrates_old_off_to_module_off(){
    let _g=ENV_LOCK.lock().unwrap_or_else(|e|e.into_inner());let mut h=H::new();h.frame(vec![]);
    h.app.settings.visible_pages[crate::app::QUOTA_PAGE]=true;h.app.quota.config.enabled=false;
    h.app.migrate_merged_switches();
    assert!(!h.app.settings.visible_pages[crate::app::QUOTA_PAGE]);
    assert!(h.app.quota.config.enabled);
    assert!(!h.app.quota_active());
}

/// 已经开着的用户不受影响。
#[test]
fn merged_switches_keep_enabled_users_unchanged(){
    let _g=ENV_LOCK.lock().unwrap_or_else(|e|e.into_inner());let mut h=H::new();h.frame(vec![]);
    let pages=h.app.settings.visible_pages;
    h.app.settings.mail_enabled=true;h.app.quota.config.enabled=true;
    h.app.migrate_merged_switches();
    assert_eq!(h.app.settings.visible_pages,pages);
    assert!(h.app.settings.mail_enabled && h.app.quota.config.enabled);
}

/// 只剩邮箱一个页面时模块关不掉：保持原来的“页面在、不收信”，不改变行为。
#[test]
fn merged_mail_switch_keeps_last_page_behavior(){
    let _g=ENV_LOCK.lock().unwrap_or_else(|e|e.into_inner());let mut h=H::new();h.frame(vec![]);
    h.app.settings.visible_pages=[false;9];h.app.settings.visible_pages[crate::app::MAIL_PAGE]=true;
    h.app.settings.mail_enabled=false;
    h.app.migrate_merged_switches();
    assert!(h.app.settings.visible_pages[crate::app::MAIL_PAGE]);
    assert!(!h.app.settings.mail_enabled,"关不掉模块时保留旧的内部开关");
    assert!(!h.app.mail_active());
}

/// 旧的分页序号（岛上入口、更新提示等按序号跳转）仍能打开合并后的页面，每个序号只属于一个侧栏项。
#[test]
fn settings_old_tab_numbers_open_merged_pages(){
    use crate::extras::settings_tab_group_for_tests as group;
    assert_eq!(group(2),"专注与健康");assert_eq!(group(3),"专注与健康");
    assert_eq!(group(4),"日程与待办");assert_eq!(group(5),"置物");
    assert_eq!(group(6),"消息提醒");assert_eq!(group(7),"邮箱");assert_eq!(group(8),"日程与待办");
    assert_eq!(group(0),"通用");assert_eq!(group(13),"外观");assert_eq!(group(14),"位置");
    assert_eq!(group(10),"页面管理");assert_eq!(group(9),"关于");
}

/// 每个分页都能正常绘制（含功能关闭时的提示），不会因为挪动布局而崩溃。
#[test]
fn settings_every_tab_renders_with_modules_on_and_off(){
    let _g=ENV_LOCK.lock().unwrap_or_else(|e|e.into_inner());
    for on in [true,false] {
        let mut h=H::new();h.app.open_settings();h.ctx.set_embed_viewports(true);
        if !on {for page in 0..9 {if page!=crate::app::MUSIC_PAGE {h.app.settings.visible_pages[page]=false;}}}
        for tab in 0..=14 {
            h.app.settings_tab=tab;
            for _ in 0..2 {let _=h.ctx.run(egui::RawInput::default(),|ctx|h.app.settings_ui(ctx,false));}
            assert_eq!(h.app.settings_tab,tab,"绘制分页 {tab} 不应自行跳转");
        }
    }
}

/// 设置窗口拉到最小（600×450）时，侧栏 13 项都在可见区内，「关于」不会被挤出去。
#[test]
fn settings_sidebar_keeps_about_visible_at_minimum_height(){
    let _g=ENV_LOCK.lock().unwrap_or_else(|e|e.into_inner());
    let mut h=H::new();h.app.open_settings();h.ctx.set_embed_viewports(false);
    let output=std::sync::Arc::new(Mutex::new(None));let capture=output.clone();
    egui::Context::set_immediate_viewport_renderer(move|ctx,mut viewport|{
        let id=viewport.ids.this;
        let raw=egui::RawInput{viewport_id:id,screen_rect:Some(Rect::from_min_size(Pos2::ZERO,egui::vec2(600.0,450.0))),
            viewports:std::iter::once((id,egui::ViewportInfo::default())).collect(),..Default::default()};
        *capture.lock().unwrap()=Some(ctx.run(raw,|ctx|(viewport.viewport_ui_cb)(ctx)));
    });
    for _ in 0..3 {let _=h.ctx.run(Default::default(),|ctx|h.app.settings_ui(ctx,false));}
    let frame=output.lock().unwrap().take().unwrap();
    for label in ["通用","天气","关于"] {
        let rect=frame.shapes.iter().find_map(|s|match &s.shape{egui::Shape::Text(t) if t.galley.text()==label && t.pos.x<180.0=>Some(t.visual_bounding_rect()),_=>None})
            .unwrap_or_else(||panic!("侧栏缺少 {label}"));
        assert!(rect.bottom()<=450.0,"{label} 超出窗口底部：{rect:?}");
    }
}

fn personal_settings_frames(h:&mut H,tab:usize,size:egui::Vec2)->Vec<(egui::FullOutput,egui::FullOutput)>{
    h.app.open_settings();h.app.settings_tab=tab;h.ctx.set_embed_viewports(false);
    let output=std::sync::Arc::new(Mutex::new(None));let capture=output.clone();
    egui::Context::set_immediate_viewport_renderer(move|ctx,mut viewport|{
        let id=viewport.ids.this;
        let raw=egui::RawInput{viewport_id:id,screen_rect:Some(Rect::from_min_size(Pos2::ZERO,size)),
            viewports:std::iter::once((id,egui::ViewportInfo{native_pixels_per_point:Some(1.0),..Default::default()})).collect(),..Default::default()};
        *capture.lock().unwrap()=Some(ctx.run(raw,|ctx|(viewport.viewport_ui_cb)(ctx)));
    });
    (0..4).map(|_|{
        let root=h.ctx.run(Default::default(),|ctx|h.app.settings_ui(ctx,false));
        (root,output.lock().unwrap().take().unwrap())
    }).collect()
}

#[test]
fn personal_task_and_calendar_entries_share_one_connection_and_shelf_is_independent(){
    let _g=ENV_LOCK.lock().unwrap_or_else(|e|e.into_inner());
    for tab in [4,8,5]{
        let mut h=H::new();
        let frames=personal_settings_frames(&mut h,tab,egui::vec2(750.0,1400.0));
        let labels:Vec<_>=frames.last().unwrap().1.shapes.iter().filter_map(|s|match &s.shape{
            egui::Shape::Text(t) if t.pos.x>180.0=>Some(t.galley.text()),_=>None,
        }).collect();
        assert_eq!(labels.iter().filter(|&&s|s=="连接飞书").count(),usize::from(tab!=5));
        assert_eq!(labels.contains(&"记录剪贴板"),tab==5);
        assert!(!labels.contains(&"待办与置物"));
    }
}

#[test]
#[ignore="Export the personal settings sidebar and connection page with mock data"]
fn personal_settings_gallery(){
    let _g=ENV_LOCK.lock().unwrap_or_else(|e|e.into_inner());
    let dest=std::path::PathBuf::from(std::env::var_os("DAOZAI_PERSONAL_SETTINGS_PREVIEW").expect("Preview directory"));
    std::fs::create_dir_all(&dest).unwrap();
    for (tab,name) in [(8,"日程与待办"),(5,"置物")]{
        let mut h=H::new();h.app.settings.settings_theme=crate::settings_theme::SettingsTheme::Light;
        let frames=personal_settings_frames(&mut h,tab,egui::vec2(750.0,980.0));
        let mut raster=crate::preview_raster::Raster::new(750,980);
        for(root,child)in frames{
            raster.update_textures(&root.textures_delta);raster.update_textures(&child.textures_delta);
            raster.buf.fill([0.96,0.96,0.97,1.0]);
            raster.draw(&h.ctx.tessellate(child.shapes,child.pixels_per_point),child.pixels_per_point);
            raster.free_textures(&root.textures_delta);raster.free_textures(&child.textures_delta);
        }
        let mut rgba=Vec::new();raster.to_rgba8(&mut rgba);
        image::save_buffer(dest.join(format!("{name}.png")),&rgba,750,980,image::ColorType::Rgba8).unwrap();
    }
}
