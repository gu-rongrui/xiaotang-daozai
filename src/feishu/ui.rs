use super::*;
use super::worker::Command;
use crate::{app::{App,Cmd,Env},paint::{self,Card,H},theme::{self,W}};
use eframe::egui::{self,Color32};

pub(crate) struct TaskDraft {guid:String,base:Value,title:String,error:String,focused:bool}
impl TaskDraft {
    fn new(task:Option<&Value>)->Self{
        Self{guid:task.map(|t|text(t,"guid").into()).unwrap_or_default(),base:task.map(task_fields).unwrap_or(Value::Null),title:task.map(|t|text(t,"summary").into()).unwrap_or_default(),error:String::new(),focused:false}
    }
    fn change(&self)->Result<Change,String>{
        if self.title.trim().is_empty(){return Err("请填写内容".into());}
        let mut patch=if self.guid.is_empty(){json!({})}else{self.base.clone()};
        patch["summary"]=json!(self.title.trim());
        Ok(Change::new(if self.guid.is_empty(){"task-create"}else{"task-update"},&self.guid,"",self.base.clone(),patch))
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
        use crate::extras::{setting_row,settings_card,settings_label};
        settings_label(ui,"飞书连接与同步");
        settings_card(ui,|ui|{
            let mut config=self.feishu.config_draft.clone().unwrap_or_else(||self.feishu.store.config.clone());
            setting_row(ui,"连接飞书","使用官方 CLI 已登录的个人账号，日程与待办共用此连接",&mut config.enabled);
            ui.separator();
            setting_row(ui,"同步待办","读取飞书默认的“我负责的”任务",&mut config.tasks);
            setting_row(ui,"同步日程","读取下方选中的飞书日历，无需填写 CalDAV 账号",&mut config.calendar);
            setting_row(ui,"允许新增和修改","在岛仔保存的更改及时同步到飞书",&mut config.writes);
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
        let writable=self.feishu.store.config.writes && !self.feishu.store.account.is_empty();
        let editing=self.feishu.editing.is_some();
        let (add,b)=paint::capsule(c,42.0,96.0,74.0,26.0,"feishu-task-add",theme::BTN,theme::BTN_HOVER);b.centered_text(79.0,109.0,if editing{"取消"}else{"＋ 加一个"},10.0,W::Regular,theme::WHITE,H::Center);
        let (refresh,b)=paint::capsule(c,124.0,96.0,62.0,26.0,"feishu-task-refresh",theme::BTN,theme::BTN_HOVER);b.centered_text(155.0,109.0,"刷新",10.0,W::Regular,theme::WHITE,H::Center);
        let refresh = refresh.on_hover_text(hint.as_str());
        if add.clicked(){if editing{self.feishu.editing=None;}else if writable{self.feishu.editing=Some(TaskDraft::new(None));}else{self.settings_tab=4;self.open_settings();}}
        if refresh.clicked(){self.feishu_send(Command::Tasks);}
        c.fill(196.0,31.0,181.0,93.0,12.0,theme::CARD);
        let draft=self.feishu.editing.as_ref();let editing=draft.is_some();
        if let Some(d)=draft{
            c.fill(203.0,36.0,167.0,20.0,7.0,Color32::from_rgb(34,34,38));
            c.stroke(203.0,36.0,167.0,20.0,7.0,1.0,paint::alpha(theme::BLUE,0.7));
            if d.guid.is_empty(){paint::plus(c,212.0,46.0,5.0,theme::BLUE,Color32::BLACK);}else{paint::pencil(c,212.0,46.0,9.0,theme::BLUE);}
        }
        if tasks.is_empty()&&!editing{c.centered_text(286.0,75.0,"飞书中暂无待办",10.0,W::Regular,theme::GRAY,H::Center);return;}
        let mut tasks=tasks;tasks.sort_by_key(|t|text(t,"completed_at").parse::<i64>().unwrap_or(0)>0);
        if let Some(d)=draft{tasks.retain(|t|text(t,"guid")!=d.guid);}
        if c.hit(196.0,31.0,181.0,93.0,"feishu-task-scroll").hovered(){let scroll=c.ui.input(|i|i.smooth_scroll_delta.y);if scroll.abs()>1.0{if scroll<0.0{self.todo_first=(self.todo_first+1).min(total.saturating_sub(3));}else{self.todo_first=self.todo_first.saturating_sub(1);}}}
        self.todo_first=self.todo_first.min(total.saturating_sub(3));
        for (i,t) in tasks.iter().skip(self.todo_first).take(if editing{2}else{3}).enumerate(){
            let y=46.0+(i+usize::from(editing)) as f32*23.0;let guid=text(t,"guid");let checked=text(t,"completed_at").parse::<i64>().unwrap_or(0)>0;
            let pending=t["daozai_pending"]==true;
            let editable=writable && task_writable(t,&self.feishu.store.account) && !pending;
            c.ring(211.0,y,5.0,1.2,if checked{theme::GREEN}else{theme::GRAY});if checked{paint::check(c,211.0,y,6.0,1.0,1.0,theme::GREEN);}
            let title_x = 223.0;
            let row_right = 370.0;
            let due_size = 8.0;
            let due = task_due_label(t);
            let title_width = due.as_ref().map_or(row_right - title_x, |label| {
                row_right - title_x - c.text_w(label, due_size, W::Regular) - 8.0
            });
            let title = c.fit(text(t, "summary"), 10.0, W::Regular, title_width);
            c.centered_text(title_x, y, &title, 10.0, W::Regular, if checked { theme::GRAY } else { theme::WHITE }, H::Left);
            if let Some(label) = &due {
                c.centered_text(row_right, y, label, due_size, W::Regular, theme::GRAY, H::Right);
            }
            if c.hit(201.0,y-9.0,18.0,18.0,("feishu-toggle",guid)).clicked()&&editable{
                let base=task_fields(t);let mut patch=base.clone();patch["completed_at"]=json!(if checked{"0".into()}else{chrono::Utc::now().timestamp_millis().to_string()});
                self.feishu_send(Command::Queue(Change::new("task-update",guid,"",base,patch)));
            }
            let row_hint = if !text(t, "daozai_blocked").is_empty() {
                "同步冲突，详情见设置"
            } else if pending {
                "已有待同步修改"
            } else if editable {
                "点击编辑内容，回车保存"
            } else {
                "只读"
            };
            let hit=c.hit(222.0,y-10.0,152.0,21.0,("feishu-edit",guid)).on_hover_text(format!("{}\n{row_hint}",text(t,"summary")));
            if hit.clicked()&&editable{self.feishu.editing=Some(TaskDraft::new(Some(t)));}
        }
        if let Some(d)=self.feishu.editing.as_ref().filter(|d|!d.error.is_empty()){
            c.centered_text(370.0,119.0,&c.fit(&d.error,7.0,W::Regular,170.0),7.0,W::Regular,theme::RED,H::Right);
        }else{c.centered_text(370.0,119.0,&format!("共 {total} 项 · 待同步 {}",self.feishu.store.queue.len()),7.0,W::Regular,theme::GRAY,H::Right);}
        let _=env;
    }
    pub(crate) fn feishu_task_inputs(&mut self,ui:&mut egui::Ui,origin:egui::Pos2,cmds:&mut Vec<Cmd>){
        let Some(d)=self.feishu.editing.as_mut() else{return;};
        let title=ui.put(egui::Rect::from_min_size(origin+egui::vec2(222.0,37.0),egui::vec2(142.0,18.0)),
            egui::TextEdit::singleline(&mut d.title).id(egui::Id::new("feishu-task-input")).frame(false).margin(egui::Margin::ZERO)
                .font(theme::font(theme::size::BODY,W::Regular)).text_color(theme::WHITE)
                .hint_text(egui::RichText::new("要做点什么？回车保存").color(theme::DIM).font(theme::font(theme::size::BODY,W::Regular))));
        if !d.focused{d.focused=true;title.request_focus();cmds.push(Cmd::Focus);}
        if (title.has_focus()||title.lost_focus()) && ui.input(|i|i.key_pressed(egui::Key::Enter)){self.save_feishu_task();}
    }
    fn save_feishu_task(&mut self){
        if self.feishu.submitted.is_some(){return;}
        let Some(mut d)=self.feishu.editing.take() else{return;};
        let result=(||{
            let change=d.change()?;
            self.feishu.submitted=Some((change.id.clone(),false));self.feishu_send(Command::Queue(change));Ok::<_,String>(())
        })();
        if let Err(e)=result{d.error=e;}else{d.error="正在保存草稿…".into();}
        self.feishu.editing=Some(d);
    }
}

fn task_due_label(task: &Value) -> Option<String> {
    let due = &task["due"];
    let timestamp = due["timestamp"].as_i64().or_else(|| text(due, "timestamp").parse().ok())?;
    if timestamp == 0 {
        return None;
    }
    let date = chrono::DateTime::from_timestamp_millis(timestamp)?.with_timezone(&chrono::Local);
    let format = if due["is_all_day"] == true { "%-m/%-d" } else { "%-m/%-d %H:%M" };
    Some(date.format(format).to_string())
}
fn config_enabled(store:&Store)->bool {store.config.enabled&&store.config.calendar}

#[cfg(test)] mod tests{
    use super::*;
    #[test] fn new_task_defaults_to_no_deadline(){
        let mut d=TaskDraft::new(None);d.title="  记一件事  ".into();
        let q=d.change().unwrap();assert_eq!(q.kind,"task-create");assert_eq!(q.patch,json!({"summary":"记一件事"}));
    }
    #[test] fn title_edit_preserves_existing_deadline_exactly(){
        let task=json!({"guid":"a","summary":"原内容","due":{"timestamp":"1794990123456","is_all_day":true},"completed_at":"0"});
        let mut d=TaskDraft::new(Some(&task));d.title="新内容".into();
        let q=d.change().unwrap();assert_eq!(q.patch["due"],task["due"]);
        assert_eq!(checked_patch(&q.base,&q.patch,&q.base).unwrap(),json!({"summary":"新内容"}));
    }
}
