use super::*;
use super::client::{Api,Cli,segment};
use chrono::{Datelike,TimeZone};
use std::{sync::mpsc::{self,Receiver,Sender},time::{Duration,Instant}};

pub(crate) enum Command {Configure(Config),Refresh(NaiveDate),Queue(Change),Discard(String),Tick(bool),Stop}
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
    let mut month=month_start(Local::now().date_naive());
    let mut next=Instant::now();let mut paused=true;
    loop {
        let Ok(cmd)=rx.recv() else{return;};
        let mut refresh=false;let mut write=false;let mut status="已就绪".to_string();let mut error=String::new();
        match cmd {
            Command::Stop=>return,
            Command::Tick(p)=>{paused=p;if paused || !store.config.enabled || Instant::now()<next{continue;}refresh=true;write=true;},
            Command::Configure(mut config)=>{
                config.interval_minutes=config.interval_minutes.clamp(15,1440);config.monthly_budget=config.monthly_budget.clamp(100,1_000_000);
                refresh=config.enabled && (!store.config.enabled || config.selected!=store.config.selected || config.cli!=store.config.cli || config.tasks!=store.config.tasks || config.calendar!=store.config.calendar);
                write=refresh || config.writes && !store.config.writes;
                store.config=config;error=store.save().err().unwrap_or_default();
            },
            Command::Refresh(m)=>{month=month_start(m);refresh=true;write=true;},
            Command::Queue(change)=>{error=store.enqueue(change).err().unwrap_or_default();write=true;},
            Command::Discard(id)=>{store.queue.retain(|q|q.id!=id);error=store.save().err().unwrap_or_default();},
        }
        // All network activity occurs here, never in the UI thread.
        if error.is_empty() && store.config.enabled && !paused && (refresh||write){
            let _=tx.send(Update{store:store.clone(),status:"正在同步…".into(),error:String::new()});ctx.request_repaint();
            let result=(||{
                let mut cli=Cli::new(&store.config.cli)?;
                bind(&mut store,&mut cli)?;
                if write && store.config.writes{flush(&mut store,&mut cli)?;}
                if refresh{sync(&mut store,&mut cli,month)?;}
                store.save()
            })();
            error=result.err().unwrap_or_default();
            status=if error.is_empty(){format!("已同步 · {}",store.last_sync)}else{"同步暂停 / 稍后重试".into()};
            next=Instant::now()+Duration::from_secs(if error.is_empty(){u64::from(store.config.interval_minutes)*60}else{600});
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
        let items=data[field].as_array().ok_or("飞书列表格式不兼容，保留上次完整缓存")?;
        rows.extend(items.iter().cloned());
        if data["has_more"]!=true{return Ok(rows);}
        let token=text(&data,"page_token");if token.is_empty()||!tokens.insert(token.to_string()){return Err("飞书分页不完整，保留上次缓存".into());}
        params["page_token"]=json!(token);
    }
    Err("飞书数据超过本次分页上限，保留上次缓存".into())
}
fn sync(store:&mut Store,api:&mut impl Api,month:NaiveDate)->Result<(),String>{
    if store.config.tasks {
        let tasks=pages(store,api,"/open-apis/task/v2/tasks",json!({"page_size":100,"type":"my_tasks","user_id_type":"open_id"}),"items")?;
        let mut ids=std::collections::HashSet::new();store.tasks=tasks.into_iter().filter(|t|!text(t,"guid").is_empty()&&ids.insert(text(t,"guid").to_string())).collect();store.save()?;
    }
    if store.config.calendar {
        let first=store.calendars.is_empty();
        let calendars=pages(store,api,"/open-apis/calendar/v4/calendars",json!({"page_size":500}),"calendar_list")?;
        store.calendars=calendars.into_iter().filter(|c|c["is_deleted"]!=true).collect();
        if first && store.config.selected.is_empty(){
            if let Some(c)=store.calendars.iter().find(|c|text(c,"type")=="primary"){store.config.selected.push(text(c,"calendar_id").into());}
        }
        store.save()?;
        let current=month_start(Local::now().date_naive());
        let months=if current==month{vec![month]}else{vec![current,month]};
        for id in store.config.selected.clone(){
            if !store.calendars.iter().any(|c|text(c,"calendar_id")==id && matches!(text(c,"role"),"reader"|"writer"|"owner")){continue;}
            for m in &months {
                let hi=m.checked_add_months(chrono::Months::new(1)).ok_or("月份范围无效")?;
                let start=Local.from_local_datetime(&m.and_hms_opt(0,0,0).unwrap()).single().ok_or("月份时区无效")?.timestamp();
                let end=Local.from_local_datetime(&hi.and_hms_opt(0,0,0).unwrap()).single().ok_or("月份时区无效")?.timestamp();
                let data=api.request(store,"GET",&format!("/open-apis/calendar/v4/calendars/{}/events/instance_view",segment(&id)),json!({"start_time":start.to_string(),"end_time":end.to_string(),"user_id_type":"open_id"}),None)?;
                let items=data["items"].as_array().ok_or("飞书日程格式不兼容")?;
                if items.len()>=1000 || data["has_more"]==true{return Err("当前月日程可能被截断，保留完整缓存；请到飞书查看".into());}
                // Replace only a successful window; an empty/error response never deletes cloud objects.
                store.events.retain(|e|e.calendar!=id || parse_event_time(&e.raw["start_time"]).is_none_or(|(t,_)|t<start||t>=end));
                for raw in items {
                    if text(raw,"event_id").is_empty(){continue;}
                    store.merge_event(&id,raw.clone());
                }
                store.save()?;
            }
        }
    }
    store.last_sync=Local::now().format("%m-%d %H:%M").to_string();Ok(())
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
        let mut cli=Cli::new("").unwrap();bind(&mut store,&mut cli).unwrap();
        sync(&mut store,&mut cli,month_start(Local::now().date_naive())).unwrap();
        assert!(!store.calendars.is_empty());assert!(store.queue.is_empty());
        println!("Read-only probe: {} calendars, {} tasks, {} event instances, {} calls; no cloud writes",store.calendars.len(),store.tasks.len(),store.events.len(),store.calls);
    }
}
