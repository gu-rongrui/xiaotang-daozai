use super::{Store,text};
use serde_json::Value;
use std::{io::{Read,Write},path::PathBuf,process::{Command,Stdio},time::{Duration,Instant}};

pub(super) trait Api {
    fn identity(&mut self,store:&Store)->Result<(String,String),String>;
    fn request(&mut self,store:&mut Store,method:&str,path:&str,params:Value,data:Option<Value>)->Result<Value,String>;
}
pub(super) struct Cli {path:PathBuf}
impl Cli {
    pub fn new(custom:&str)->Result<Self,String>{
        let path=if custom.trim().is_empty(){
            std::env::var_os("APPDATA").map(PathBuf::from).ok_or("找不到飞书 CLI，请在设置指定 lark-cli.exe")?.join("npm/node_modules/@larksuite/cli/bin/lark-cli.exe")
        }else{PathBuf::from(custom.trim())};
        if !path.is_absolute() || path.extension().is_none_or(|x|!x.eq_ignore_ascii_case("exe")) || !path.is_file(){return Err("找不到 lark-cli.exe，请安装官方 CLI 或指定其完整路径".into());}
        Ok(Self{path})
    }
    fn run(&self,profile:&str,args:&[String],data:Option<Value>)->Result<Value,String>{
        let mut cmd=Command::new(&self.path);
        if !profile.is_empty(){cmd.args(["--profile",profile]);}
        cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)] {use std::os::windows::process::CommandExt;cmd.creation_flags(0x08000000);}
        let mut child=cmd.spawn().map_err(|_|"无法启动飞书 CLI，请检查安装和文件权限")?;
        let output=child.stdout.take().ok_or("飞书 CLI 输出不可用")?;
        let reader=std::thread::spawn(move||{let mut bytes=Vec::new();let result=output.take(16*1024*1024+1).read_to_end(&mut bytes);(result,bytes)});
        if let Some(value)=data {
            let input=serde_json::to_vec(&value).map_err(|_|"请求序列化失败")?;
            if child.stdin.take().ok_or("飞书 CLI 输入不可用")?.write_all(&input).is_err(){let _=child.kill();let _=child.wait();let _=reader.join();return Err("无法向飞书 CLI 提交请求".into());}
        }else{drop(child.stdin.take());}
        let start=Instant::now();
        loop {
            match child.try_wait(){
                Ok(Some(_))=>break,
                Ok(None) if start.elapsed()<Duration::from_secs(40)=>std::thread::sleep(Duration::from_millis(40)),
                _=>{let _=child.kill();let _=child.wait();let _=reader.join();return Err("飞书请求超时或中断，修改已保留，稍后重试".into());}
            }
        }
        let (read,bytes)=reader.join().map_err(|_|"读取飞书 CLI 失败")?;
        if read.is_err() || bytes.len()>16*1024*1024{return Err("飞书返回内容过大或不完整，已停止本次同步".into());}
        let value:Value=serde_json::from_slice(&bytes).map_err(|_|"飞书 CLI 没有返回有效 JSON，请检查版本和登录状态")?;
        if value["ok"]==false {
            let code=value["error"]["code"].as_i64().unwrap_or(0);
            return Err(match code {
                99991403=>"飞书月度 API 额度已用尽，缓存和修改已保留".into(),
                99991400=>"飞书请求受到限流，稍后重试".into(),
                _=>format!("飞书请求失败（代码 {code}），请检查登录、权限或网络；本地修改已保留"),
            });
        }
        Ok(value)
    }
}
impl Api for Cli {
    fn identity(&mut self,store:&Store)->Result<(String,String),String>{
        let v=self.run(&store.profile,&["auth".into(),"status".into(),"--json".into()],None)?;
        let user=&v["identities"]["user"];
        if user["available"]!=true || text(user,"openId").is_empty(){return Err("飞书 CLI 尚未登录，请先在终端完成用户授权".into());}
        if text(&v,"brand")!="feishu"{return Err("当前 CLI 配置不是国内飞书账号，请检查授权配置".into());}
        Ok((text(user,"openId").into(),text(&v,"appId").into()))
    }
    fn request(&mut self,store:&mut Store,method:&str,path:&str,params:Value,data:Option<Value>)->Result<Value,String>{
        if !matches!(method,"GET"|"POST"|"PATCH"){return Err("本版本不允许云端删除".into());}
        store.reserve_call()?;
        let mut args=vec!["api".into(),method.into(),path.into(),"--as".into(),"user".into(),"--params".into(),params.to_string()];
        if data.is_some(){args.extend(["--data".into(),"-".into()]);}
        let v=self.run(&store.profile,&args,data)?;
        if v["ok"]!=true{return Err("CLI 响应格式不兼容，已停止本次同步".into());}
        Ok(v["data"].clone())
    }
}
pub(super) fn segment(id:&str)->String {
    id.bytes().map(|b|if b.is_ascii_alphanumeric() || b"-_.~".contains(&b){(b as char).to_string()}else{format!("%{b:02X}")}).collect()
}
