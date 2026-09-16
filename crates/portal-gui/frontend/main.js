const invoke = window.__TAURI__?.core?.invoke;
const $ = (id) => document.getElementById(id);
let demo = true;
function toast(text){ $('toast').textContent=text; $('toast').classList.add('show'); setTimeout(()=>$('toast').classList.remove('show'),2600); }
function render(snapshot){ $('status-text').textContent=snapshot.online?'已上线':'已下线'; $('user-name').textContent=snapshot.username; $('download-total').textContent=snapshot.download_total; $('upload-total').textContent=snapshot.upload_total; $('download-rate').textContent=snapshot.download_rate; $('upload-rate').textContent=snapshot.upload_rate; const s=snapshot.sessions[0]; if(s){$('session-id').textContent=s.id;$('session-ip').textContent=s.ip;$('session-duration').textContent=s.duration;} }
async function load(){ if(demo){ render(await invoke?.('demo_snapshot') ?? {online:true,username:'demo-user',download_total:'5.52 GB',upload_total:'312.4 MB',download_rate:'等待后台计费更新',upload_rate:'等待后台计费更新',sessions:[{id:'682516164',ip:'10.90.156.148',duration:'00:12:42'}]}); } }
document.querySelectorAll('.nav').forEach(btn=>btn.addEventListener('click',()=>{document.querySelectorAll('.nav').forEach(x=>x.classList.remove('active'));btn.classList.add('active');document.querySelectorAll('.page').forEach(x=>x.classList.add('hidden'));$(`${btn.dataset.page}`).classList.remove('hidden');$('page-title').textContent=btn.dataset.page==='dashboard'?'认证概览':btn.dataset.page==='settings'?'设置':'诊断';}));
$('refresh').onclick=()=>{toast('已刷新；后台无新增计费数据时速率保持等待更新');load()};
$('disconnect').onclick=()=>{toast('Demo：已模拟下线，真实版本会按 radacctid 请求后台并确认会话消失');$('status-text').textContent='已下线';};
$('session-disconnect').onclick=()=> $('disconnect').click();
$('open-site').onclick=()=>{const url=$('auth-url').value; if(window.__TAURI__?.shell?.open){window.__TAURI__.shell.open(url)}else{toast('Demo：真实版本会打开认证网站 '+url)}};
$('save-settings').onclick=()=>{toast($('one-session').checked?'设置已保存；仅一次会话不会保存账号':'设置已保存')};
load();
