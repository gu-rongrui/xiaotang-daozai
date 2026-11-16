//! Personal Feishu connection. Cloud data and pending writes never enter the legacy local files.
mod client;
pub(crate) mod worker;
mod ui;

use chrono::{Local, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

pub(crate) use worker::Service;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Config {
    pub enabled: bool,
    pub writes: bool,
    pub tasks: bool,
    pub calendar: bool,
    pub selected: Vec<String>,
    pub cli: String,
    pub interval_minutes: u32,
    pub monthly_budget: u32,
}
impl Default for Config {
    fn default()->Self {Self{enabled:false,writes:false,tasks:true,calendar:true,selected:vec![],cli:String::new(),interval_minutes:60,monthly_budget:8000}}
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct SavedEvent {pub calendar:String,pub raw:Value}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Change {
    pub id:String,
    pub kind:String,
    pub target:String,
    pub calendar:String,
    pub base:Value,
    pub patch:Value,
    pub created:i64,
    #[serde(default)] pub attempted:bool,
    #[serde(default)] pub blocked:String,
}
impl Change {
    pub fn new(kind:&str,target:&str,calendar:&str,base:Value,patch:Value)->Self {
        use std::sync::atomic::{AtomicU64,Ordering};
        static NEXT:AtomicU64=AtomicU64::new(0);
        let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        Self{id:format!("dz-{:08x}-{:032x}-{:08x}",std::process::id(),now,NEXT.fetch_add(1,Ordering::Relaxed)),kind:kind.into(),target:target.into(),calendar:calendar.into(),base,patch,created:chrono::Utc::now().timestamp(),attempted:false,blocked:String::new()}
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Store {
    pub version:u32,
    pub config:Config,
    pub account:String,
    pub profile:String,
    pub tasks:Vec<Value>,
    pub calendars:Vec<Value>,
    pub events:Vec<SavedEvent>,
    pub queue:Vec<Change>,
    pub budget_month:String,
    pub calls:u32,
    pub last_sync:String,
}
impl Store {
    fn path()->PathBuf {crate::model::config_dir().join("feishu-sync.json")}
    pub fn load()->Result<Self,String> {
        match std::fs::read(Self::path()) {
            Ok(bytes)=>{let s:Self=serde_json::from_slice(&bytes).map_err(|_|"飞书缓存无法解析，已停止同步；请保留文件并恢复备份".to_string())?;
                if s.version!=1{return Err("飞书缓存版本不兼容，已停止同步".into());}Ok(s)},
            Err(e) if e.kind()==std::io::ErrorKind::NotFound=>Ok(Self{version:1,..Default::default()}),
            Err(_)=>Err("无法读取飞书缓存，已停止同步".into()),
        }
    }
    pub fn save(&self)->Result<(),String> {
        let path=Self::path();std::fs::create_dir_all(path.parent().unwrap()).map_err(|_|"无法创建飞书数据目录")?;
        let bytes=serde_json::to_vec(self).map_err(|_|"飞书缓存保存失败")?;
        crate::model::atomic_write(&path,&bytes).map_err(|_|"飞书缓存保存失败，已停止云端写入".into())
    }
    pub fn reserve_call(&mut self)->Result<(),String> {
        let month=chrono::Utc::now().with_timezone(&Local).format("%Y-%m").to_string();
        if self.budget_month!=month{self.budget_month=month;self.calls=0;}
        if self.calls>=self.config.monthly_budget{return Err("本机请求预算已用尽，保留缓存和待同步修改；可在设置调整预算".into());}
        self.calls+=1;self.save()
    }
    pub fn calendar_name(&self,id:&str)->String {
        self.calendars.iter().find(|c|text(c,"calendar_id")==id).map(|c|{
            let alias=text(c,"summary_alias");if alias.is_empty(){text(c,"summary").to_string()}else{alias.to_string()}
        }).unwrap_or_else(||"飞书日历".into())
    }
    pub fn calendar_writable(&self,id:&str)->bool {
        self.calendars.iter().any(|c|text(c,"calendar_id")==id && matches!(text(c,"role"),"owner"|"writer") && matches!(text(c,"type"),"primary"|"shared") && c["is_third_party"]!=true)
    }
    pub fn merge_event(&mut self,calendar:&str,raw:Value){
        let normal=text(&raw,"recurrence").is_empty() && text(&raw,"recurring_event_id").is_empty() && raw["is_exception"]!=true && text(&raw,"event_id").ends_with("_0");
        self.events.retain(|e|e.calendar!=calendar || e.raw["event_id"]!=raw["event_id"] || !normal && e.raw["start_time"]!=raw["start_time"]);
        self.events.push(SavedEvent{calendar:calendar.into(),raw});
    }
    pub fn view_tasks(&self)->Vec<Value>{
        let mut tasks=self.tasks.clone();
        for q in self.queue.iter().filter(|q|q.kind.starts_with("task-")){
            let id=if q.target.is_empty(){q.id.as_str()}else{q.target.as_str()};
            let mut task=tasks.iter().find(|t|text(t,"guid")==id).cloned().unwrap_or_else(||json!({"guid":id}));
            for (key,value) in q.patch.as_object().into_iter().flatten(){if q.base.get(key)!=Some(value){task[key]=value.clone();}}
            task["daozai_pending"]=json!(true);task["daozai_blocked"]=json!(q.blocked);
            tasks.retain(|t|text(t,"guid")!=id);tasks.push(task);
        }
        tasks
    }
    pub fn enqueue(&mut self,change:Change)->Result<(),String>{
        if !self.config.enabled || !self.config.writes || self.account.is_empty(){return Err("请先连接飞书并开启云端写入".into());}
        if self.queue.iter().any(|q|q.target==change.target && q.calendar==change.calendar && !q.target.is_empty()){
            return Err("该条目已有待同步修改，请同步或处理冲突后再编辑".into());
        }
        self.queue.push(change);if let Err(e)=self.save(){self.queue.pop();return Err(e);}Ok(())
    }
}

pub(crate) fn text<'a>(v:&'a Value,key:&str)->&'a str {v[key].as_str().unwrap_or("")}
pub(crate) fn task_fields(v:&Value)->Value {json!({"summary":v["summary"],"due":v.get("due").filter(|x|!x.is_null()).cloned().unwrap_or(Value::Null),"completed_at":if text(v,"completed_at").is_empty(){"0"}else{text(v,"completed_at")}})}
pub(crate) fn event_fields(v:&Value)->Value {json!({"summary":v["summary"],"start_time":v["start_time"],"end_time":v["end_time"]})}
pub(crate) fn task_writable(v:&Value,account:&str)->bool {
    text(v,"repeat_rule").is_empty() && text(v,"parent_task_guid").is_empty() && v["subtask_count"].as_i64().unwrap_or(0)==0 && v.get("custom_complete").is_none_or(|x|x.is_null()||x.as_object().is_some_and(|o|o.is_empty())) &&
        v["members"].as_array().is_some_and(|members|{
            let owners:Vec<_>=members.iter().filter(|m|text(m,"role")=="assignee").collect();
            owners.len()==1 && text(owners[0],"id")==account
        })
}
pub(crate) fn event_writable(v:&Value,calendar:&str)->bool {
    v["daozai_pending"]!=true && text(v,"recurrence").is_empty() && text(v,"recurring_event_id").is_empty() && v["is_exception"]!=true && text(v,"event_id").ends_with("_0") && text(v,"organizer_calendar_id")==calendar
}

// Compare only fields the user actually changed. Never reconstruct a cloud object from the UI model.
pub(crate) fn checked_patch(base:&Value,desired:&Value,remote:&Value)->Result<Value,String>{
    let mut out=serde_json::Map::new();
    for (key,value) in desired.as_object().ok_or("修改内容无效")? {
        if base.get(key)==Some(value){continue;}
        if remote.get(key)!=base.get(key) && remote.get(key)!=Some(value){return Err(format!("云端的 {key} 已变化，本地草稿已保留"));}
        if remote.get(key)!=Some(value){out.insert(key.clone(),value.clone());}
    }
    Ok(Value::Object(out))
}

pub(crate) fn parse_event_time(v:&Value)->Option<(i64,bool)> {
    if let Some(date)=v["date"].as_str(){
        // All-day values are civil dates, not UTC instants. Keep their local calendar dates.
        let day=NaiveDate::parse_from_str(date,"%Y-%m-%d").ok()?;
        return Some((Local.from_local_datetime(&day.and_hms_opt(0,0,0)?).single()?.timestamp(),true));
    }
    Some((text(v,"timestamp").parse().ok()?,false))
}
pub(crate) struct ViewEvent {pub meeting:crate::mail::Meeting,pub calendar:String,pub name:String,pub color:eframe::egui::Color32,pub raw:Value}
impl Store {
    pub fn view_events(&self)->Vec<ViewEvent>{
        let mut events=self.events.clone();
        for q in self.queue.iter().filter(|q|q.kind.starts_with("event-")){
            let id=if q.target.is_empty(){q.id.as_str()}else{q.target.as_str()};
            let mut raw=events.iter().find(|e|e.calendar==q.calendar&&text(&e.raw,"event_id")==id).map(|e|e.raw.clone()).unwrap_or_else(||json!({"event_id":id}));
            for (key,value) in q.patch.as_object().into_iter().flatten(){if q.base.get(key)!=Some(value){raw[key]=value.clone();}}
            raw["daozai_pending"]=json!(true);raw["daozai_blocked"]=json!(q.blocked);
            events.retain(|e|e.calendar!=q.calendar||text(&e.raw,"event_id")!=id);events.push(SavedEvent{calendar:q.calendar.clone(),raw});
        }
        events.iter().filter(|e|self.config.selected.contains(&e.calendar)).filter_map(|e|{
            if text(&e.raw,"status")=="cancelled"{return None;}
            let (start,all_day)=parse_event_time(&e.raw["start_time"])?;let (end,_)=parse_event_time(&e.raw["end_time"])?;
            let cal=self.calendars.iter().find(|c|text(c,"calendar_id")==e.calendar)?;
            let color=e.raw["color"].as_i64().filter(|c|*c!=0 && *c!=-1).or_else(||cal["color"].as_i64()).unwrap_or(0x70a2ff);
            let color=if color<0 {0x70a2ff}else{color as u32};
            let name=if e.raw["daozai_pending"]==true{format!("{} · {}",self.calendar_name(&e.calendar),if text(&e.raw,"daozai_blocked").is_empty(){"待同步"}else{"冲突"})}else{self.calendar_name(&e.calendar)};
            Some(ViewEvent{meeting:crate::mail::Meeting{uid:format!("feishu|{}|{}|{start}",e.calendar,text(&e.raw,"event_id")),title:text(&e.raw,"summary").into(),start,end:Some(end),all_day,at:chrono::DateTime::from_timestamp(start,0)?.with_timezone(&Local).format("%m-%d %H:%M").to_string()},calendar:e.calendar.clone(),name,color:eframe::egui::Color32::from_rgb((color>>16) as u8,(color>>8) as u8,color as u8),raw:e.raw.clone()})
        }).collect()
    }
}

pub(crate) struct State {
    pub store:Store,
    pub events:Vec<ViewEvent>,
    pub error:String,
    pub status:String,
    pub service:Option<Service>,
    pub editing:Option<ui::TaskDraft>,
    pub config_draft:Option<Config>,
    pub calendar_target:String,
    pub calendar_base:Value,
    pub calendar_event:String,
    pub submitted:Option<(String,bool)>,
    pub reminded:std::collections::HashSet<String>,
    pub last_tick:f64,
}
impl Default for State {
    fn default()->Self {
        let (store,error)=match Store::load(){Ok(s)=>(s,String::new()),Err(e)=>(Store::default(),e)};
        let events=store.view_events();
        let reminded=std::fs::read(crate::model::config_dir().join("feishu-reminders.json")).ok().and_then(|v|serde_json::from_slice(&v).ok()).unwrap_or_default();
        Self{store,events,error,status:"尚未连接".into(),service:None,editing:None,config_draft:None,calendar_target:String::new(),calendar_base:Value::Null,calendar_event:String::new(),submitted:None,reminded,last_tick:-10.0}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn conflict_preserves_unedited_remote_fields(){
        let base=json!({"summary":"old","due":null,"completed_at":"0"});
        let desired=json!({"summary":"mine","due":null,"completed_at":"0"});
        let remote=json!({"summary":"old","due":{"timestamp":"123"},"completed_at":"0"});
        assert_eq!(checked_patch(&base,&desired,&remote).unwrap(),json!({"summary":"mine"}));
        assert!(checked_patch(&base,&desired,&json!({"summary":"theirs"})).is_err());
        assert_eq!(checked_patch(&base,&desired,&desired).unwrap(),json!({}));
    }
    #[test] fn all_day_keeps_civil_date_and_exclusive_end(){
        let (a,all)=parse_event_time(&json!({"date":"2026-11-16"})).unwrap();
        let (b,_)=parse_event_time(&json!({"date":"2026-11-17"})).unwrap();
        assert!(all);assert!(b>a);
        assert_eq!(chrono::DateTime::from_timestamp(a,0).unwrap().with_timezone(&Local).format("%Y-%m-%d").to_string(),"2026-11-16");
    }
    #[test] fn recurring_and_collaborative_tasks_are_read_only(){
        let mut t=json!({"members":[{"id":"me","role":"assignee"}]});assert!(task_writable(&t,"me"));
        t["repeat_rule"]=json!("FREQ=DAILY");assert!(!task_writable(&t,"me"));
        assert!(!task_writable(&json!({"members":[{"id":"other","role":"assignee"}]}),"me"));
    }
    #[test] fn offline_queue_is_durable_and_uncertain_creates_do_not_repeat(){
        let _guard=crate::ui_tests::ENV_LOCK.lock().unwrap_or_else(|e|e.into_inner());
        let old=std::env::var_os("DAOZAI_DATA_DIR");
        let dir=std::env::current_dir().unwrap().join("target").join(Change::new("test","","",Value::Null,Value::Null).id);
        std::env::set_var("DAOZAI_DATA_DIR",&dir);
        let mut store=Store{version:1,account:"me".into(),profile:"app".into(),config:Config{enabled:true,writes:true,..Default::default()},..Default::default()};
        let change=Change::new("task-create","","",Value::Null,json!({"summary":"offline draft"}));
        let id=change.id.clone();store.enqueue(change).unwrap();
        let restored=Store::load().unwrap();assert_eq!(restored.queue.len(),1);assert_eq!(restored.queue[0].id,id);
        assert!(!restored.queue[0].attempted);assert_eq!(restored.queue[0].patch["summary"],"offline draft");
        assert_eq!(restored.view_tasks()[0]["summary"],"offline draft");assert_eq!(restored.view_tasks()[0]["daozai_pending"],true);
        store.config.writes=false;assert!(store.enqueue(Change::new("task-create","","",Value::Null,json!({"summary":"denied"}))).is_err());
        if let Some(old)=old{std::env::set_var("DAOZAI_DATA_DIR",old);}else{std::env::remove_var("DAOZAI_DATA_DIR");}
    }
    #[test] fn exhausted_budget_stops_before_network(){
        let mut store=Store{config:Config{monthly_budget:4,..Default::default()},budget_month:Local::now().format("%Y-%m").to_string(),calls:4,..Default::default()};
        assert!(store.reserve_call().is_err());assert_eq!(store.calls,4);
    }
    #[test] fn moved_single_events_replace_old_cache_but_repeated_instances_stay_distinct(){
        let mut s=Store::default();
        s.merge_event("cal",json!({"event_id":"id_0","start_time":{"timestamp":"1"}}));
        s.merge_event("cal",json!({"event_id":"id_0","start_time":{"timestamp":"2"}}));
        assert_eq!(s.events.len(),1);assert_eq!(s.events[0].raw["start_time"]["timestamp"],"2");
        for stamp in ["3","4"]{s.merge_event("cal",json!({"event_id":"repeat_0","recurrence":"FREQ=DAILY","start_time":{"timestamp":stamp}}));}
        assert_eq!(s.events.len(),3);
    }
}
