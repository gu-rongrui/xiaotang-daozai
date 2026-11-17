use super::*;
use super::client::{Api,Cli,segment};
use chrono::{Datelike,TimeZone};
use std::{sync::mpsc::{self,Receiver,Sender},time::{Duration,Instant}};

pub(crate) enum Command {Configure(Config),Refresh(NaiveDate),Calendar(NaiveDate,bool),Tasks,Queue(Change),Discard(String),Tick(bool),Stop}
#[derive(Clone,Copy)]
enum Scope {All(NaiveDate),Background,Calendar(NaiveDate),Tasks}
#[derive(Default)]
struct Freshness {calendars:Option<Instant>,windows:std::collections::HashMap<(String,NaiveDate),Instant>}
fn recent(time:Option<Instant>,seconds:u64)->bool {time.is_some_and(|t|t.elapsed()<Duration::from_secs(seconds))}
fn list_items<'a>(data:&'a Value,field:&str)->Result<&'a [Value],String>{
    // Successful Feishu empty-list responses can omit the list field (data = {}).
    // Null/non-object data and a wrongly typed list are not complete snapshots.
    if !data.is_object(){return Err("飞书列表响应不完整，保留上次缓存".into());}
    match data.get(field){
        Some(Value::Array(items))=>Ok(items),
        None if data["has_more"]!=true && data.as_object().unwrap().keys().all(|k|matches!(k.as_str(),"has_more"|"page_token"|"sync_token"))=>Ok(&[]),
        _=>Err("飞书列表格式不兼容，保留上次完整缓存".into()),
    }
}
pub(crate) struct Update {pub store:Store,pub status:String,pub error:String}
pub(crate) struct Service {tx:Sender<Command>,rx:Receiver<Update>}
impl Service {
    pub fn start(store:Store,ctx:eframe::egui::Context)->Self {
        let (tx,commands)=mpsc::channel();let (updates,rx)=mpsc::channel();
        std::thread::Builder::new().name("daozai-feishu".into()).spawn(move||run(store,commands,updates,ctx)).ok();
        Self{tx,rx}
    }
    pub fn send(&self,cmd:Command){let _=self.tx.send(cmd);}
    pub fn poll(&self)->Option<Update>{self.rx.try_recv().ok()}
}
impl Drop for Service {fn drop(&mut self){let _=self.tx.send(Command::Stop);}}

fn month_start(day:NaiveDate)->NaiveDate {day.with_day(1).unwrap()}
fn run(mut store:Store,rx:Receiver<Command>,tx:Sender<Update>,ctx:eframe::egui::Context){
    let mut next=Instant::now();let mut paused=true;
    let mut fresh=Freshness::default();let mut deferred=None;let mut last_manual=None;let mut failures=0u32;let mut blocked=None;let mut pause_error=String::new();
    loop {
        let Some(mut cmd)=deferred.take().or_else(||rx.recv().ok()) else{return;};
        // Navigation settles before requesting; keep the last month in a rapid sequence.
        if matches!(cmd,Command::Calendar(_,false)){
            while let Ok(new)=rx.recv_timeout(Duration::from_millis(250)){
                if matches!(new,Command::Calendar(_,false)){cmd=new;}else{deferred=Some(new);break;}
            }
        }
        let mut scope=None;let mut force=false;let mut write=false;let mut status="已就绪".to_string();let mut error=String::new();
        match cmd {
            Command::Stop=>return,
            Command::Tick(p)=>{
                let resumed=paused&&!p;paused=p;
                if blocked.as_deref().is_some_and(|m|m!="manual" && m!=Local::now().format("%Y-%m").to_string()){blocked=None;next=Instant::now();}
                if paused || blocked.is_some() || !store.config.enabled{continue;}
                if Instant::now()>=next{scope=Some(Scope::Background);}
                else if !(resumed && store.config.writes && store.queue.iter().any(|q|q.blocked.is_empty())){continue;}
                write=true;
            },
            Command::Configure(mut config)=>{
                config.interval_minutes=config.interval_minutes.clamp(15,1440);config.monthly_budget=config.monthly_budget.clamp(100,1_000_000);
                let refresh=config.enabled && (!store.config.enabled || config.selected!=store.config.selected || config.cli!=store.config.cli || config.tasks!=store.config.tasks || config.calendar!=store.config.calendar);
                if refresh{fresh=Freshness::default();scope=Some(Scope::Background);force=true;}
                write=refresh || config.writes && !store.config.writes;
                next=Instant::now();failures=0;blocked=None;
                store.config=config;error=store.save().err().unwrap_or_default();
            },
            Command::Refresh(m)=>{if recent(last_manual,5){continue;}last_manual=Some(Instant::now());scope=Some(Scope::All(month_start(m)));force=true;write=true;},
            Command::Tasks=>{if recent(last_manual,5){continue;}last_manual=Some(Instant::now());scope=Some(Scope::Tasks);force=true;write=true;},
            Command::Calendar(m,f)=>{if f && recent(last_manual,5){continue;}if f{last_manual=Some(Instant::now());}scope=Some(Scope::Calendar(month_start(m)));force=f;write=f;},
            Command::Queue(change)=>{error=store.enqueue(change).err().unwrap_or_default();write=true;},
            Command::Discard(id)=>{store.queue.retain(|q|q.id!=id);error=store.save().err().unwrap_or_default();},
        }
        if force{blocked=None;}
        if blocked.is_some(){error=pause_error.clone();status="同步暂停 · 请处理后立即同步".into();}
        // All network activity occurs here, never in the UI thread.
        if error.is_empty() && store.config.enabled && !paused && blocked.is_none() && (scope.is_some()||write){
            let _=tx.send(Update{store:store.clone(),status:"正在同步…".into(),error:String::new()});ctx.request_repaint();
            let result=(||{
                let mut cli=Cli::new(&store.config.cli)?;
                bind(&mut store,&mut cli)?;
                if write && store.config.writes{flush(&mut store,&mut cli)?;}
                if let Some(scope)=scope{sync(&mut store,&mut cli,scope,force,&mut fresh)?;}
                store.save()
            })();
            error=result.err().unwrap_or_default();
            status=if error.is_empty(){format!("已同步 · {}",store.last_sync)}else{"同步暂停 / 稍后重试".into()};
            if error.is_empty(){
                failures=0;
                // Writes and browsing must not postpone the regular remote-change check.
                if matches!(scope,Some(Scope::All(_)|Scope::Background)){next=Instant::now()+Duration::from_secs(u64::from(store.config.interval_minutes)*60);}
            }else{
                failures=failures.saturating_add(1);
                next=Instant::now()+Duration::from_secs(match failures{1=>60,2=>300,3=>900,_=>3600});
                if error.contains("预算已用尽") || error.contains("月度 API 额度已用尽"){blocked=Some(Local::now().format("%Y-%m").to_string());status="额度暂停 · 下月或手动重试".into();}
                else if error.contains("格式") || error.contains("响应不完整") || error.contains("身份与缓存不一致") || error.contains("尚未登录"){blocked=Some("manual".into());status="同步暂停 · 请处理后立即同步".into();}
                if blocked.is_some(){pause_error=error.clone();}
            }
        }else if !store.config.enabled{status="飞书同步已关闭，保留缓存和待同步修改".into();}
        let _=tx.send(Update{store:store.clone(),status,error});ctx.request_repaint();
    }
}
fn bind(store:&mut Store,api:&mut impl Api)->Result<(),String>{
    let (account,profile)=api.identity(store)?;
    if !store.account.is_empty() && (store.account!=account || store.profile!=profile){return Err("CLI 授权身份与缓存不一致，已暂停读写；请恢复原账号，历史数据不会跨账号上传".into());}
    store.account=account;store.profile=profile;store.save()
}
fn pages(store:&mut Store,api:&mut impl Api,path:&str,mut params:Value,field:&str)->Result<Vec<Value>,String>{
    let mut rows=vec![];let mut tokens=std::collections::HashSet::new();
    for _ in 0..200 {
        let data=api.request(store,"GET",path,params.clone(),None)?;
        let items=list_items(&data,field)?;
        rows.extend(items.iter().cloned());
        if data["has_more"]!=true{return Ok(rows);}
        let token=text(&data,"page_token");if token.is_empty()||!tokens.insert(token.to_string()){return Err("飞书分页不完整，保留上次缓存".into());}
        params["page_token"]=json!(token);
    }
    Err("飞书数据超过本次分页上限，保留上次缓存".into())
}
fn sync(store:&mut Store,api:&mut impl Api,scope:Scope,force:bool,fresh:&mut Freshness)->Result<(),String>{
    let mut loaded=false;
    if store.config.tasks && !matches!(scope,Scope::Calendar(_)) {
        let tasks=pages(store,api,"/open-apis/task/v2/tasks",json!({"page_size":100,"type":"my_tasks","user_id_type":"open_id"}),"items")?;
        loaded=true;
        let mut ids=std::collections::HashSet::new();store.tasks=tasks.into_iter().filter(|t|!text(t,"guid").is_empty()&&ids.insert(text(t,"guid").to_string())).collect();store.save()?;
    }
    if store.config.calendar && !matches!(scope,Scope::Tasks) {
        let first=store.calendars.is_empty();
        if (force && matches!(scope,Scope::All(_))) || !recent(fresh.calendars,6*3600){
        let calendars=pages(store,api,"/open-apis/calendar/v4/calendars",json!({"page_size":500}),"calendar_list")?;
        store.calendars=calendars.into_iter().filter(|c|c["is_deleted"]!=true).collect();
        if first && store.config.selected.is_empty(){
            if let Some(c)=store.calendars.iter().find(|c|text(c,"type")=="primary"){store.config.selected.push(text(c,"calendar_id").into());}
        }
        store.save()?;
        fresh.calendars=Some(Instant::now());
        loaded=true;
        }
        let current=month_start(Local::now().date_naive());
        let months=match scope{Scope::All(m) if m!=current=>vec![current,m],Scope::All(m)|Scope::Calendar(m)=>vec![m],_=>vec![current]};
        for id in store.config.selected.clone(){
            if !store.calendars.iter().any(|c|text(c,"calendar_id")==id && matches!(text(c,"role"),"reader"|"writer"|"owner")){continue;}
            for m in &months {
                let key=(id.clone(),*m);
                if !force && matches!(scope,Scope::Calendar(_)) && recent(fresh.windows.get(&key).copied(),600){continue;}
                let hi=m.checked_add_months(chrono::Months::new(1)).ok_or("月份范围无效")?;
                let start=Local.from_local_datetime(&m.and_hms_opt(0,0,0).unwrap()).single().ok_or("月份时区无效")?.timestamp();
                let end=Local.from_local_datetime(&hi.and_hms_opt(0,0,0).unwrap()).single().ok_or("月份时区无效")?.timestamp();
                let data=api.request(store,"GET",&format!("/open-apis/calendar/v4/calendars/{}/events/instance_view",segment(&id)),json!({"start_time":start.to_string(),"end_time":end.to_string(),"user_id_type":"open_id"}),None)?;
                let items=list_items(&data,"items")?;
                loaded=true;
                if items.len()>=1000 || data["has_more"]==true{return Err("当前月日程可能被截断，保留完整缓存；请到飞书查看".into());}
                // Replace only a successful window; an empty/error response never deletes cloud objects.
                store.events.retain(|e|e.calendar!=id || parse_event_time(&e.raw["start_time"]).is_none_or(|(t,_)|t<start||t>=end));
                for raw in items {
                    if text(raw,"event_id").is_empty(){continue;}
                    store.merge_event(&id,raw.clone());
                }
                store.save()?;
                fresh.windows.insert(key,Instant::now());
            }
        }
    }
    if loaded{store.last_sync=Local::now().format("%m-%d %H:%M").to_string();}Ok(())
}
fn execute(store:&mut Store,api:&mut impl Api,q:&Change)->Result<Value,String>{
    if q.kind=="task-create" {
        let mut body=q.patch.clone();body["client_token"]=json!(q.id);body["members"]=json!([{"id":store.account,"role":"assignee","type":"user"}]);
        return api.request(store,"POST","/open-apis/task/v2/tasks",json!({"user_id_type":"open_id"}),Some(body)).map(|v|v["task"].clone());
    }
    if q.kind=="event-create" {
        if !store.calendar_writable(&q.calendar){return Err("目标日历不可写，草稿已保留".into());}
        return api.request(store,"POST",&format!("/open-apis/calendar/v4/calendars/{}/events",segment(&q.calendar)),json!({"idempotency_key":q.id,"user_id_type":"open_id"}),Some(q.patch.clone())).map(|v|v["event"].clone());
    }
    let task=q.kind=="task-update";
    let path=if task{format!("/open-apis/task/v2/tasks/{}",segment(&q.target))}else{format!("/open-apis/calendar/v4/calendars/{}/events/{}",segment(&q.calendar),segment(&q.target))};
    let data=api.request(store,"GET",&path,json!({"user_id_type":"open_id"}),None)?;
    let remote=&data[if task{"task"}else{"event"}];
    if remote.is_null() || (!task && text(remote,"status")=="cancelled"){return Err("云端条目已删除或不可访问，草稿已保留".into());}
    if task && !task_writable(remote,&store.account) || !task && (!store.calendar_writable(&q.calendar)||!event_writable(remote,&q.calendar)){return Err("条目为重复内容或当前权限不允许编辑，草稿已保留".into());}
    let remote_fields=if task{task_fields(remote)}else{event_fields(remote)};
    let patch=checked_patch(&q.base,&q.patch,&remote_fields)?;
    if patch.as_object().is_some_and(|p|p.is_empty()){return Ok(remote.clone());}
    let body=if task{json!({"update_fields":patch.as_object().unwrap().keys().collect::<Vec<_>>(),"task":patch})}else{patch};
    api.request(store,"PATCH",&path,json!({"user_id_type":"open_id"}),Some(body)).map(|v|v[if task{"task"}else{"event"}].clone())
}
fn flush(store:&mut Store,api:&mut impl Api)->Result<(),String>{
    for id in store.queue.iter().filter(|q|q.blocked.is_empty()).map(|q|q.id.clone()).collect::<Vec<_>>() {
        let index=store.queue.iter().position(|q|q.id==id).unwrap();
        let mut q=store.queue[index].clone();
        // Task idempotency is only guaranteed for a short window, and the CLI may refresh
        // its token. After any uncertain POST, never blindly retry (including after restart).
        if q.kind.ends_with("create") && q.attempted {
            store.queue[index].blocked="创建结果待核对，已停止自动重试以避免重复；请到飞书检查后处理草稿".into();store.save()?;continue;
        }
        q.attempted=true;store.queue[index].attempted=true;store.save()?;
        match execute(store,api,&q){
            Ok(value)=>{
                if value.is_null(){return Err("飞书写入响应不完整，操作已保留待核对".into());}
                if q.kind.starts_with("task") {
                    let guid=text(&value,"guid");if guid.is_empty(){return Err("飞书任务身份缺失，操作已保留".into());}
                    store.tasks.retain(|v|text(v,"guid")!=guid);store.tasks.push(value);
                }else{
                    let eid=text(&value,"event_id");if eid.is_empty(){return Err("飞书日程身份缺失，操作已保留".into());}
                    store.merge_event(&q.calendar,value);
                }
                store.queue.remove(index);store.save()?;
            },
            Err(e) if e.contains("已变化")||e.contains("草稿已保留")=>{store.queue[index].blocked=e;store.save()?;},
            Err(e)=>return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)] mod tests {
    use super::*;
    struct Mock {responses:Vec<Value>,calls:Vec<(String,String,Option<Value>)>}
    impl Api for Mock {
        fn identity(&mut self,_:&Store)->Result<(String,String),String>{Ok(("me".into(),"app".into()))}
        fn request(&mut self,_:&mut Store,m:&str,p:&str,_:Value,b:Option<Value>)->Result<Value,String>{self.calls.push((m.into(),p.into(),b));if self.responses.is_empty(){return Err("unexpected write".into());}Ok(self.responses.remove(0))}
    }
    #[test] fn conflict_never_sends_patch(){
        let mut store=Store{account:"me".into(),..Default::default()};
        let base=json!({"summary":"before","due":null,"completed_at":"0"});
        let q=Change::new("task-update","guid","",base,json!({"summary":"mine"}));
        let mut mock=Mock{responses:vec![json!({"task":{"guid":"guid","summary":"theirs","members":[{"id":"me","role":"assignee"}]}})],calls:vec![]};
        assert!(execute(&mut store,&mut mock,&q).unwrap_err().contains("已变化"));assert_eq!(mock.calls.len(),1);assert_eq!(mock.calls[0].0,"GET");
    }
    #[test] fn create_retries_keep_id_and_assign_self(){
        let mut store=Store{account:"me".into(),..Default::default()};
        let q=Change::new("task-create","","",Value::Null,json!({"summary":"test"}));
        let mut mock=Mock{responses:vec![json!({"task":{"guid":"a"}}),json!({"task":{"guid":"a"}})],calls:vec![]};
        execute(&mut store,&mut mock,&q).unwrap();execute(&mut store,&mut mock,&q).unwrap();
        assert_eq!(mock.calls[0].2,mock.calls[1].2);assert_eq!(mock.calls[0].2.as_ref().unwrap()["members"][0]["id"],"me");
        assert!((32..=128).contains(&q.id.len()));
    }
    #[test] fn partial_pagination_is_not_a_complete_empty_list(){
        let mut store=Store::default();let mut mock=Mock{responses:vec![json!({"items":[],"has_more":true,"page_token":""})],calls:vec![]};
        assert!(pages(&mut store,&mut mock,"/x",json!({}),"items").is_err());
    }
    #[test] fn successful_empty_lists_and_malformed_responses_are_distinct(){
        for value in [json!({}),json!({"items":[]})]{assert!(list_items(&value,"items").unwrap().is_empty());}
        for value in [Value::Null,json!([]),json!({"items":null}),json!({"items":{}}),json!({"has_more":true}),json!({"unexpected":"payload"})]{assert!(list_items(&value,"items").is_err());}
    }
    #[test] fn calendar_navigation_reuses_windows_and_does_not_reload_tasks_or_metadata(){
        let mut store=Store{version:1,config:Config{selected:vec!["cal".into()],..Default::default()},calendars:vec![json!({"calendar_id":"cal","role":"owner","type":"primary"})],..Default::default()};
        let month=month_start(Local::now().date_naive());
        let mut fresh=Freshness{calendars:Some(Instant::now()),..Default::default()};
        let mut api=Mock{responses:vec![json!({}),json!({})],calls:vec![]};
        sync(&mut store,&mut api,Scope::Calendar(month),false,&mut fresh).unwrap();
        sync(&mut store,&mut api,Scope::Calendar(month),false,&mut fresh).unwrap();
        assert_eq!(api.calls.len(),1);assert!(api.calls[0].1.ends_with("/instance_view"));
        sync(&mut store,&mut api,Scope::Calendar(month.checked_add_months(chrono::Months::new(1)).unwrap()),false,&mut fresh).unwrap();
        assert_eq!(api.calls.len(),2);
    }
    #[test] fn background_refresh_reuses_metadata_and_skips_browsed_historic_months(){
        let mut store=Store{version:1,config:Config{selected:vec!["cal".into()],..Default::default()},calendars:vec![json!({"calendar_id":"cal","role":"reader"})],..Default::default()};
        let old=NaiveDate::from_ymd_opt(2020,1,1).unwrap();
        let mut fresh=Freshness{calendars:Some(Instant::now()),windows:[(("cal".into(),old),Instant::now())].into(),..Default::default()};
        let mut api=Mock{responses:vec![json!({"items":[]}),json!({})],calls:vec![]};
        sync(&mut store,&mut api,Scope::Background,false,&mut fresh).unwrap();
        assert_eq!(api.calls.len(),2);assert!(api.calls[0].1.contains("/task/v2/tasks"));assert!(api.calls[1].1.ends_with("/instance_view"));
        assert!(fresh.windows.contains_key(&("cal".into(),month_start(Local::now().date_naive()))));
    }
    #[test] fn malformed_calendar_keeps_cache_and_successful_empty_clears_only_its_window(){
        let month=month_start(Local::now().date_naive());
        let raw=json!({"event_id":"a_0","start_time":{"date":month.to_string()},"end_time":{"date":month.succ_opt().unwrap().to_string()}});
        let mut store=Store{version:1,config:Config{selected:vec!["cal".into()],..Default::default()},calendars:vec![json!({"calendar_id":"cal","role":"owner"})],events:vec![SavedEvent{calendar:"cal".into(),raw:raw.clone()},SavedEvent{calendar:"other".into(),raw}],..Default::default()};
        let mut fresh=Freshness{calendars:Some(Instant::now()),..Default::default()};
        let mut api=Mock{responses:vec![Value::Null,json!({})],calls:vec![]};
        assert!(sync(&mut store,&mut api,Scope::Calendar(month),true,&mut fresh).is_err());assert_eq!(store.events.len(),2);
        sync(&mut store,&mut api,Scope::Calendar(month),true,&mut fresh).unwrap();assert_eq!(store.events.len(),1);assert_eq!(store.events[0].calendar,"other");
    }
    #[test] fn account_switch_cannot_upload_cached_operations(){
        let mut store=Store{account:"another-user".into(),profile:"app".into(),..Default::default()};
        let mut mock=Mock{responses:vec![],calls:vec![]};
        assert!(bind(&mut store,&mut mock).unwrap_err().contains("身份与缓存不一致"));assert!(mock.calls.is_empty());
    }
    #[test] fn patch_does_not_clear_remote_description_or_collaboration_fields(){
        let mut store=Store{account:"me".into(),..Default::default()};
        let remote=json!({"guid":"guid","summary":"before","completed_at":"0","members":[{"id":"me","role":"assignee"}],"description":"keep-rich-content","due":null});
        let q=Change::new("task-update","guid","",task_fields(&remote),json!({"summary":"after"}));
        let mut mock=Mock{responses:vec![json!({"task":remote}),json!({"task":{"guid":"guid"}})],calls:vec![]};
        execute(&mut store,&mut mock,&q).unwrap();
        assert_eq!(mock.calls[1].2.as_ref().unwrap(),&json!({"update_fields":["summary"],"task":{"summary":"after"}}));
    }
    #[test]
    #[ignore="Explicit read-only probe against the user's authorized Feishu account"]
    fn live_read_only_probe(){
        let dir=std::env::var_os("DAOZAI_LIVE_READ_DATA").expect("Set an isolated live-read directory");
        std::env::set_var("DAOZAI_DATA_DIR",dir);
        let mut store=Store{version:1,config:Config{enabled:true,writes:false,..Default::default()},..Default::default()};
        if let Ok(ids)=std::env::var("DAOZAI_LIVE_CALENDARS"){store.config.selected=serde_json::from_str(&ids).expect("JSON calendar IDs");}
        let mut cli=Cli::new("").unwrap();bind(&mut store,&mut cli).unwrap();
        sync(&mut store,&mut cli,Scope::Background,false,&mut Freshness::default()).unwrap();
        assert!(!store.calendars.is_empty());assert!(store.queue.is_empty());
        println!("Read-only probe: {} calendars, {} tasks, {} event instances, {} calls; no cloud writes",store.calendars.len(),store.tasks.len(),store.events.len(),store.calls);
    }
}
