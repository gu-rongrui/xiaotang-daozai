use super::*;
use super::worker::Command;
use crate::{app::{App,Cmd,Env},paint::{self,Card,H},theme::{self,W}};
use eframe::egui::{self,Color32};

pub(crate) struct TaskDraft {guid:String,base:Value,title:String,due:String,all_day:bool,error:String,focused:bool}
impl TaskDraft {
    fn new(task:Option<&Value>)->Self{
        let due=task.and_then(|t|text(&t["due"],"timestamp").parse::<i64>().ok()).and_then(chrono::DateTime::from_timestamp_millis).map(|d|d.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
        Self{guid:task.map(|t|text(t,"guid").into()).unwrap_or_default(),base:task.map(task_fields).unwrap_or(Value::Null),title:task.map(|t|text(t,"summary").into()).unwrap_or_default(),due,all_day:task.is_some_and(|t|t["due"]["is_all_day"]==true),error:String::new(),focused:false}
    }
}
impl App {
    pub(crate) fn feishu_tick(&mut self,ctx:&egui::Context,env:&Env){
        if cfg!(test) || !self.feishu.error.is_empty() && self.feishu.store.version!=1{return;}
        if self.feishu.service.is_none(){self.feishu.service=Some(Service::start(self.feishu.store.clone(),ctx.clone()));}
        while let Some(update)=self.feishu.service.as_ref().and_then(Service::poll){
            if let Some((id,calendar))=&self.feishu.submitted {
                if update.store.queue.iter().any(|q|&q.id==id){
                    if *calendar{self.calendar.draft=None;}else{self.feishu.editing=None;}
                    self.feishu.submitted=None;
                }else if !update.error.is_empty(){
                    if *calendar{if let Some(d)=&mut self.calendar.draft{d.error=update.error.clone();}}
                    else if let Some(d)=&mut self.feishu.editing{d.error=update.error.clone();}
                    self.feishu.submitted=None;
                }
            }
            self.feishu.store=update.store;self.feishu.events=self.feishu.store.view_events();self.feishu.status=update.status;self.feishu.error=update.error;
        }
        if env.t-self.feishu.last_tick>=5.0 {
            self.feishu.last_tick=env.t;
            let paused=env.screen_locked || env.network==Some(false) || !(self.module_on(crate::app::TODO_PAGE)||self.module_on(crate::app::CALENDAR_PAGE));
            self.feishu_send(Command::Tick(paused));
        }
    }
    pub(crate) fn feishu_send(&self,command:Command){if let Some(service)=&self.feishu.service{service.send(command);}}
    pub(crate) fn feishu_refresh(&self){self.feishu_send(Command::Refresh(self.calendar.view.month));}
    pub(crate) fn feishu_task_reminders(&mut self){
        if !self.feishu.store.config.enabled || !self.feishu.store.config.tasks || !self.module_on(crate::app::TODO_PAGE){return;}
        let now=chrono::Utc::now().timestamp_millis();
        let due:Vec<_>=self.feishu.store.tasks.iter().filter(|t|text(t,"completed_at").parse::<i64>().unwrap_or(0)==0).filter_map(|t|{
            let stamp=text(&t["due"],"timestamp").parse::<i64>().ok()?;
            let key=format!("{}|{}|{stamp}",self.feishu.store.account,text(t,"guid"));
            (t["due"]["is_all_day"]!=true && now>=stamp && now<stamp+60_000 && !self.feishu.reminded.contains(&key)).then(||(key,text(t,"summary").to_string()))
        }).collect();
        for (key,title) in due {
            self.feishu.reminded.insert(key);
            if let Ok(bytes)=serde_json::to_vec(&self.feishu.reminded){let _=crate::model::atomic_write(&crate::model::config_dir().join("feishu-reminders.json"),&bytes);}
            self.alert("飞书待办到时间了",title,crate::app::TODO_PAGE,true);
        }
    }
    pub(crate) fn feishu_settings(&mut self,ui:&mut egui::Ui){
        ui.group(|ui|{
            ui.strong("飞书个人同步 · API");
            let mut config=self.feishu.config_draft.clone().unwrap_or_else(||self.feishu.store.config.clone());
            ui.checkbox(&mut config.enabled,"连接现有飞书 CLI 登录");
            ui.horizontal(|ui|{ui.checkbox(&mut config.tasks,"待办使用飞书");ui.checkbox(&mut config.calendar,"日程使用飞书 API");});
            ui.checkbox(&mut config.writes,"允许在岛仔新增和修改飞书内容");
            ui.small("首次连接只读取；开启云端写入后，保存操作进入本机队列。已有本地数据不会自动上传。");
            ui.horizontal(|ui|{ui.label("后台刷新（分钟）");ui.add(egui::DragValue::new(&mut config.interval_minutes).range(15..=1440));});
            ui.horizontal(|ui|{ui.label("本机每月请求预算");ui.add(egui::DragValue::new(&mut config.monthly_budget).range(100..=1_000_000));});
            ui.small(format!("本机本月已计入 {} 次请求（含失败尝试）；不是飞书账号总用量。",self.feishu.store.calls));
            ui.small("后台按间隔读取待办和本月日程；翻页复用 10 分钟缓存，日历列表缓存 6 小时。保存修改及时提交，立即同步可强制刷新。");
            egui::CollapsingHeader::new("CLI 路径").show(ui,|ui|{ui.add(egui::TextEdit::singleline(&mut config.cli).hint_text("留空使用官方 npm 安装的 lark-cli.exe"));});
            if config.calendar {
                ui.label("需要同步的日历（首次默认选择主日历）");
                for cal in &self.feishu.store.calendars {
                    let id=text(cal,"calendar_id");let mut on=config.selected.iter().any(|s|s==id);
                    if ui.checkbox(&mut on,format!("{} · {}",self.feishu.store.calendar_name(id),text(cal,"role"))).changed(){
                        if on{config.selected.push(id.into());}else{config.selected.retain(|s|s!=id);}
                    }
                }
            }
            if ui.button("应用同步设置").clicked(){self.feishu_send(Command::Configure(config.clone()));self.feishu.config_draft=None;}
            else if serde_json::to_string(&config).ok()!=serde_json::to_string(&self.feishu.store.config).ok(){self.feishu.config_draft=Some(config);}
            ui.horizontal(|ui|{if ui.button("立即同步").clicked(){self.feishu_refresh();}ui.label(&self.feishu.status);});
            if !self.feishu.error.is_empty(){ui.colored_label(Color32::from_rgb(220,95,85),&self.feishu.error);}
            if config_enabled(&self.feishu.store){ui.small("飞书 API 启用时，本定制版暂停原飞书 CalDAV 连接；其他来源保持可用。");}
            if !self.feishu.store.queue.is_empty(){
                ui.separator();ui.label(format!("待同步 / 待处理草稿：{}",self.feishu.store.queue.len()));
                for q in self.feishu.store.queue.clone(){
                    ui.group(|ui|{
                        ui.label(text(&q.patch,"summary"));
                        ui.small(if q.blocked.is_empty(){"等待联网同步"}else{&q.blocked});
                        if ui.button("放弃此本地草稿（不删除云端内容）").clicked(){self.feishu_send(Command::Discard(q.id));}
                    });
                }
            }
        });
    }
    pub(crate) fn page_feishu_todo(&mut self,c:&Card,env:&Env){
        let tasks=self.feishu.store.view_tasks();let total=tasks.len();let done=tasks.iter().filter(|t|text(t,"completed_at").parse::<i64>().unwrap_or(0)>0).count();
        c.text(42.0,61.0,&format!("{done}/{total}"),theme::size::HERO,W::Bold,theme::WHITE,H::Left);
        c.text(42.0,78.0,"飞书 · 我负责的",theme::size::SMALL,W::Regular,theme::GRAY,H::Left);
        let hint=if self.feishu.error.is_empty(){&self.feishu.status}else{&self.feishu.error};
        c.centered_text(42.0,18.0,&c.fit(hint,8.0,W::Regular,330.0),8.0,W::Regular,theme::GRAY,H::Left);
        let writable=self.feishu.store.config.writes && !self.feishu.store.account.is_empty();
        if self.feishu.editing.is_some(){
            c.fill(196.0,31.0,181.0,93.0,12.0,theme::CARD);
            c.text(202.0,32.0,"内容",8.0,W::Regular,theme::GRAY,H::Left);
            c.text(202.0,63.0,"截止：年-月-日 时:分（可留空）",7.3,W::Regular,theme::GRAY,H::Left);
            let (cancel,b)=paint::capsule(c,42.0,96.0,65.0,26.0,"feishu-task-cancel",theme::BTN,theme::BTN_HOVER);b.centered_text(74.5,109.0,"取消",10.0,W::Regular,theme::WHITE,H::Center);
            let (save,b)=paint::capsule(c,116.0,96.0,70.0,26.0,"feishu-task-save",theme::BTN,theme::BTN_HOVER);b.centered_text(151.0,109.0,"保存到飞书",9.5,W::Regular,theme::BLUE,H::Center);
            if let Some(d)=&self.feishu.editing{c.centered_text(202.0,116.0,&c.fit(&d.error,7.3,W::Regular,170.0),7.3,W::Regular,theme::RED,H::Left);}
            if cancel.clicked(){self.feishu.editing=None;}else if save.clicked(){self.save_feishu_task();}return;
        }
        let (add,b)=paint::capsule(c,42.0,96.0,74.0,26.0,"feishu-task-add",theme::BTN,theme::BTN_HOVER);b.centered_text(79.0,109.0,"＋ 加一个",10.0,W::Regular,theme::WHITE,H::Center);
        let (refresh,b)=paint::capsule(c,124.0,96.0,62.0,26.0,"feishu-task-refresh",theme::BTN,theme::BTN_HOVER);b.centered_text(155.0,109.0,"刷新",10.0,W::Regular,theme::WHITE,H::Center);
        if add.clicked(){if writable{self.feishu.editing=Some(TaskDraft::new(None));}else{self.settings_tab=4;self.open_settings();}}
        if refresh.clicked(){self.feishu_send(Command::Tasks);}
        c.fill(196.0,31.0,181.0,93.0,12.0,theme::CARD);
        if tasks.is_empty(){c.centered_text(286.0,75.0,"飞书中暂无待办",10.0,W::Regular,theme::GRAY,H::Center);return;}
        let mut tasks=tasks;tasks.sort_by_key(|t|text(t,"completed_at").parse::<i64>().unwrap_or(0)>0);
        if c.hit(196.0,31.0,181.0,93.0,"feishu-task-scroll").hovered(){let scroll=c.ui.input(|i|i.smooth_scroll_delta.y);if scroll.abs()>1.0{if scroll<0.0{self.todo_first=(self.todo_first+1).min(total.saturating_sub(3));}else{self.todo_first=self.todo_first.saturating_sub(1);}}}
        self.todo_first=self.todo_first.min(total.saturating_sub(3));
        for (i,t) in tasks.iter().skip(self.todo_first).take(3).enumerate(){
            let y=46.0+i as f32*23.0;let guid=text(t,"guid");let checked=text(t,"completed_at").parse::<i64>().unwrap_or(0)>0;
            let pending=t["daozai_pending"]==true;
            let editable=writable && task_writable(t,&self.feishu.store.account) && !pending;
            c.ring(211.0,y,5.0,1.2,if checked{theme::GREEN}else{theme::GRAY});if checked{paint::check(c,211.0,y,6.0,1.0,1.0,theme::GREEN);}
            c.centered_text(223.0,y,&c.fit(text(t,"summary"),10.0,W::Regular,119.0),10.0,W::Regular,if checked{theme::GRAY}else{theme::WHITE},H::Left);
            c.centered_text(361.0,y,if !text(t,"daozai_blocked").is_empty(){"冲突"}else if pending{"待同步"}else if editable{"编辑"}else{"只读"},7.5,W::Regular,theme::BLUE,H::Center);
            if c.hit(201.0,y-9.0,18.0,18.0,("feishu-toggle",guid)).clicked()&&editable{
                let base=task_fields(t);let mut patch=base.clone();patch["completed_at"]=json!(if checked{"0".into()}else{chrono::Utc::now().timestamp_millis().to_string()});
                self.feishu_send(Command::Queue(Change::new("task-update",guid,"",base,patch)));
            }
            let hit=c.hit(222.0,y-10.0,152.0,21.0,("feishu-edit",guid)).on_hover_text(format!("{}\n{}",text(t,"summary"),if pending{"已有待同步修改"}else{"点击编辑内容和截止时间"}));
            if hit.clicked()&&editable{self.feishu.editing=Some(TaskDraft::new(Some(t)));}
        }
        c.centered_text(370.0,119.0,&format!("共 {total} 项 · 待同步 {}",self.feishu.store.queue.len()),7.0,W::Regular,theme::GRAY,H::Right);
        let _=env;
    }
    pub(crate) fn feishu_task_inputs(&mut self,ui:&mut egui::Ui,origin:egui::Pos2,cmds:&mut Vec<Cmd>){
        let Some(d)=self.feishu.editing.as_mut() else{return;};
        let title=ui.put(egui::Rect::from_min_size(origin+egui::vec2(202.0,36.0),egui::vec2(168.0,20.0)),egui::TextEdit::singleline(&mut d.title).frame(false).font(theme::font(10.5,W::Regular)).text_color(theme::WHITE));
        ui.put(egui::Rect::from_min_size(origin+egui::vec2(202.0,69.0),egui::vec2(168.0,20.0)),egui::TextEdit::singleline(&mut d.due).frame(false).font(theme::font(9.0,W::Regular)).text_color(theme::WHITE));
        if !d.focused{d.focused=true;title.request_focus();cmds.push(Cmd::Focus);}
    }
    fn save_feishu_task(&mut self){
        if self.feishu.submitted.is_some(){return;}
        let Some(mut d)=self.feishu.editing.take() else{return;};
        let result=(||{
            if d.title.trim().is_empty(){return Err("请填写内容".to_string());}
            let due=if d.due.trim().is_empty(){Value::Null}else{
                let dt=chrono::NaiveDateTime::parse_from_str(d.due.trim(),"%Y-%m-%d %H:%M").map_err(|_|"时间格式：2026-11-16 18:30".to_string())?;
                let dt=Local.from_local_datetime(&dt).single().ok_or("该时间有歧义，请调整")?;
                json!({"timestamp":dt.timestamp_millis().to_string(),"is_all_day":d.all_day})
            };
            let mut patch=if d.guid.is_empty(){json!({"summary":d.title.trim(),"due":due})}else{let mut v=d.base.clone();v["summary"]=json!(d.title.trim());v["due"]=due;v};
            if d.guid.is_empty()&&patch["due"].is_null(){patch.as_object_mut().unwrap().remove("due");}
            let change=Change::new(if d.guid.is_empty(){"task-create"}else{"task-update"},&d.guid,"",d.base.clone(),patch);
            self.feishu.submitted=Some((change.id.clone(),false));self.feishu_send(Command::Queue(change));Ok::<_,String>(())
        })();
        if let Err(e)=result{d.error=e;}else{d.error="正在保存草稿…".into();}
        self.feishu.editing=Some(d);
    }
}
fn config_enabled(store:&Store)->bool {store.config.enabled&&store.config.calendar}
