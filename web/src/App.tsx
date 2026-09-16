import { useEffect, useRef, useState } from 'react'
import type { FormEvent, ReactNode } from 'react'
import { active, api, ApiError } from './api'
import type { Login, Status } from './api'

function Icon({name, size=20}:{name:string;size?:number}) {
  const paths:Record<string,ReactNode> = {
    grid:<><rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/></>,
    key:<><circle cx="8" cy="9" r="5"/><path d="m12 13 8 8m-4-4 3-3m-6 0 3-3"/></>,
    arrow:<><path d="M5 12h14m-6-6 6 6-6 6"/></>,
    check:<path d="m5 12 4 4L19 6"/>,
    refresh:<><path d="M20 8a8 8 0 1 0 0 8M20 3v5h-5"/></>,
    external:<><path d="M14 3h7v7m0-7L10 14M10 3H4v17h16v-6"/></>,
    shield:<><path d="m12 3 8 3v6c0 5-8 9-8 9s-8-4-8-9V6l8-3Z"/><path d="m8 11 3 3 5-5"/></>,
    exit:<><path d="M10 4H4v16h6m3-8h8m-4-4 4 4-4 4"/></>,
    terminal:<><path d="m5 7 5 5-5 5m8 0h6"/></>,
    link:<><path d="m9 8 3-3a5 5 0 0 1 7 7l-3 3m-1 1-3 3a5 5 0 0 1-7-7l3-3m0 7 8-8"/></>,
  }
  return <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.65" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{paths[name]??paths.grid}</svg>
}
const labels:Record<string,string> = {starting:'正在启动登录',waiting:'等待账号授权',submitting:'正在完成登录',succeeded:'账号登录成功',failed:'登录未完成',cancelled:'已取消登录',expired:'授权已过期'}
function message(error:unknown) { return error instanceof Error ? error.message : '操作失败，请重试' }

export default function App() {
  // Secrets only live in component memory: never URLs, cookies, storage or build-time env.
  const [token, setToken] = useState('')
  const [candidate, setCandidate] = useState('')
  const [data, setData] = useState<Status|null>(null)
  const [login, setLogin] = useState<Login|null>(null)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState('')
  const [section, setSection] = useState('account')
  const [method, setMethod] = useState('claudeai')
  const [code, setCode] = useState('')
  const [notice, setNotice] = useState('')
  const [confirmLogout, setConfirmLogout] = useState(false)
  const generation = useRef(0)

  function lock() { generation.current++; setToken(''); setCandidate(''); setData(null); setLogin(null); setCode(''); setError(''); setNotice(''); setConfirmLogout(false); setBusy('') }
  function fail(e:unknown) { if(e instanceof ApiError && e.status===401) { lock(); setError('管理令牌无效或已变更，请重新解锁。') } else setError(message(e)) }
  async function unlock(event:FormEvent) {
    event.preventDefault(); setBusy('unlock'); setError(''); const key=candidate.trim()
    try { const result=await api<Status>(key,'/status'); generation.current++; setData(result); setLogin(result.login); setToken(key); setCandidate('') }
    catch(e) { setError(e instanceof ApiError && e.status===401?'管理令牌不正确，请检查后重试。':message(e)) }
    finally { setBusy('') }
  }
  async function refresh() {
    const version=generation.current
    setBusy('refresh'); setError('')
    try { const result=await api<Status>(token,'/status'); if(version!==generation.current)return; setData(result); setLogin(result.login) }
    catch(e){ if(version===generation.current)fail(e) } finally { if(version===generation.current)setBusy('') }
  }
  useEffect(() => {
    if(!token||!active(login))return
    let stopped=false; let timer:ReturnType<typeof setTimeout>
    async function poll() {
      try {
        const result=await api<{login:Login|null}>(token,'/oauth')
        if(stopped)return
        setLogin(result.login)
        if(!active(result.login)) {
          setCode('')
          const status=await api<Status>(token,'/status')
          if(!stopped)setData(status)
          return
        }
      } catch(e) { if(!stopped)fail(e) }
      if(!stopped)timer=setTimeout(poll,1500)
    }
    timer=setTimeout(poll,700)
    return () => { stopped=true; clearTimeout(timer) }
  // Poll follows the login id; transitions are delivered by this same loop.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  },[token,login?.id])
  async function action(name:string, run:()=>Promise<void>) {
    const version=generation.current
    setBusy(name); setError(''); setNotice('')
    try { await run() } catch(e) { if(version===generation.current)fail(e) } finally { if(version===generation.current)setBusy('') }
  }
  async function start(event:FormEvent) { event.preventDefault(); await action('start',async()=>{const result=await api<Login>(token,'/oauth/start','POST',{method});setCode('');setLogin(result)}) }
  async function submit(event:FormEvent) { event.preventDefault(); if(!login)return; await action('code',async()=>{await api(token,`/oauth/${login.id}/code`,'POST',{code:code.trim()});setCode('');setLogin({...login,status:'submitting'})}) }
  async function cancel() { if(!login)return; await action('cancel',async()=>{await api(token,`/oauth/${login.id}`,'DELETE');setCode('');setLogin({...login,status:'cancelled',authorization_url:null})}) }
  async function logout() { await action('logout',async()=>{await api(token,'/logout','POST');setConfirmLogout(false);setLogin(null);setData(await api<Status>(token,'/status'));setNotice('服务账号已退出，实例绑定已保留。')}) }
  async function copy(text:string) { try { await navigator.clipboard.writeText(text);setNotice('已复制到剪贴板') } catch { setError('无法自动复制，请手动选择文本复制。') } }

  if(!token||!data) return <div className="gate"><div className="gate-brand"><Logo/><span>BRIDGE</span><span className="tag">CONTROL</span></div><main className="gate-card"><div className="symbol"><Icon name="key" size={27}/></div><p className="eyebrow">ADMIN ACCESS</p><h1>连接你的 AI 工作流</h1><p className="muted">使用管理令牌解锁控制台，管理 Claude 账号与 Messages 服务。</p><form onSubmit={unlock}><label htmlFor="admin-token">Admin token</label><input id="admin-token" type="password" autoComplete="off" autoFocus placeholder="输入 BRIDGE_ADMIN_TOKEN" value={candidate} onChange={e=>setCandidate(e.target.value)} required/><button className="primary full" disabled={!!busy||!candidate.trim()}>{busy?'正在验证…':'解锁控制台'}<Icon name="arrow" size={18}/></button></form>{error&&<div className="alert" role="alert">{error}</div>}<div className="gate-note"><Icon name="shield"/><span>令牌仅保留在当前页面内存中，刷新后需重新输入。</span></div></main><footer>CLAUDE CODE → ANTHROPIC MESSAGES<span>本地服务 · 独立凭据</span></footer></div>

  const connected=data.account.logged_in
  const binding=data.account.binding
  const running=active(login)
  const base=window.location.origin
  return <div className="shell"><aside><div className="brand"><Logo/><div><strong>Bridge</strong><small>CLAUDE CONTROL</small></div></div><div className="nav-caption">工作空间</div><nav><button className={section==='account'?'selected':''} onClick={()=>setSection('account')}><Icon name="grid"/>账号与连接<Icon name="arrow" size={16}/></button><button className={section==='api'?'selected':''} onClick={()=>setSection('api')}><Icon name="terminal"/>API 接入</button></nav><div className="side-bottom"><div className="secure"><Icon name="shield"/><div>管理员会话<small>仅当前页面有效</small></div></div><button className="lock" onClick={lock}><Icon name="exit"/>锁定控制台</button><div className="side-version">Bridge v{data.service.version}<span>AXUM / REACT</span></div></div></aside><div className="workspace"><header><div className="breadcrumb">工作空间 <span>/</span> <strong>{section==='account'?'账号与连接':'API 接入'}</strong></div><div className="header-right"><span className="live-dot"/>服务在线<span className="avatar">AD</span></div></header><main className="content"><div className="page-title"><div><p className="eyebrow">{section==='account'?'ACCOUNT & CONNECTION':'MESSAGES API'}</p><h1>{section==='account'?'账号与连接':'一个端点，接入你的应用'}</h1><p className="muted">{section==='account'?'一个实例绑定一个 Claude 账号，为你的应用提供 Messages API。':'兼容 Messages 核心协议，支持流式输出与标准工具往返。'}</p></div><button className="secondary" onClick={refresh} disabled={!!busy}><Icon name="refresh" size={17}/>{busy==='refresh'?'正在刷新…':'刷新状态'}</button></div>{error&&<div className="alert" role="alert">{error}</div>}{notice&&<div className="notice" role="status"><Icon name="check" size={17}/>{notice}</div>}
    <div className="metrics"><Metric label="服务账号" value={connected?'已连接':binding?'已绑定 · 待登录':'等待连接'} icon="link" detail={connected?(data.account.email??data.account.method??'Claude 账号'):binding?(binding.email??'请重新授权绑定账号'):'首次 OAuth 登录后固定绑定账号'} good={connected}/><Metric label="可用请求槽位" value={`${running?0:data.service.available_slots} / ${data.service.max_concurrency}`} icon="grid" detail={running?'授权期间暂停接收模型请求':'每个请求使用独立 CLI 进程'}/><Metric label="接口鉴权" value={data.service.api_key_required?'API key 已启用':'仅本机 / 未设密钥'} icon="shield" detail="与管理令牌独立配置"/></div>
    {section==='account'?<div className="account-layout"><section className="panel account-panel"><div className="panel-heading"><div className="provider-mark">✳</div><div><h2>Claude 账号</h2><p>通过 CLI 原生 RPC 登录</p></div><span className={`pill ${connected?'green':''}`}><i/>{connected?'已连接':'未连接'}</span></div>{connected?<div className="account-info"><div className="identity"><div className="identity-avatar">{(data.account.email??'C')[0].toUpperCase()}</div><div><strong>{data.account.email??'Claude 账号'}</strong><p>{data.account.organization??'个人账号'}</p></div></div><dl><div><dt>认证方式</dt><dd>{data.account.method??'OAuth'}</dd></div><div><dt>订阅类型</dt><dd>{data.account.subscription??'—'}</dd></div><div><dt>凭据存储</dt><dd>redb 本地数据库</dd></div></dl>{confirmLogout?<div className="logout-confirm"><p>退出后暂停模型请求，实例仍绑定此账号。重新授权后即可恢复。</p><button className="danger" onClick={logout} disabled={!!busy}>确认退出服务账号</button><button className="text-button" onClick={()=>setConfirmLogout(false)}>取消</button></div>:<button className="text-button danger-text" disabled={running||!!busy} onClick={()=>setConfirmLogout(true)}>退出服务账号</button>}</div>:<div className="empty-account"><div className="empty-orbit"><Icon name="link" size={32}/><span/></div><h3>{binding?'绑定账号已退出':'还没有连接账号'}</h3><p>{binding?<>本实例仍绑定 {binding.email??binding.account_id??'原账号'}。<br/>请使用此账号重新授权。</>:<>首次 OAuth 登录后固定绑定账号。<br/>凭据保存在服务端 redb。</>}</p></div>}<div className="panel-foot"><Icon name="shield" size={17}/><span>凭据持久化到 redb，OAuth token 不会返回浏览器。</span></div></section>
    <section className="panel login-panel"><div className="section-top"><span className="step-number">01</span><div><h2>{binding?'重新授权绑定账号':'绑定服务账号'}</h2><p>由 Claude CLI 生成官方授权链接</p></div></div>{!running?<form onSubmit={start}><label>认证方式</label><div className="segmented"><button type="button" className={method==='claudeai'?'chosen':''} onClick={()=>setMethod('claudeai')}>Claude 订阅<span>Pro / Max / Team</span></button><button type="button" className={method==='console'?'chosen':''} onClick={()=>setMethod('console')}>Anthropic Console<span>API 用量计费</span></button></div><button className="primary full" disabled={!!busy}>{busy==='start'?'正在启动…':binding?'重新授权此账号':'开始 OAuth 登录'}<Icon name="arrow" size={18}/></button><p className="fine-print">{binding?'仅接受绑定账号，退出或重启不会解除绑定。其他账号请使用独立实例。':'首次登录将绑定本实例；其他账号需使用独立实例。'}授权期间暂停模型请求。</p></form>:<div className="flow"><div className="flow-state"><span className="spinner"/><div><strong>{labels[login!.status]}</strong><small>有效期至 {new Date(login!.expires_at).toLocaleTimeString('zh-CN',{hour:'2-digit',minute:'2-digit'})}</small></div></div>{login!.authorization_url&&<><a className="primary full" href={login!.authorization_url} target="_blank" rel="noopener noreferrer">打开官方授权页面<Icon name="external" size={17}/></a><button className="text-button full" onClick={()=>copy(login!.authorization_url!)}>复制授权链接</button><div className="divider"><span>授权完成后</span></div><p className="muted small">将官方授权页面提供的完整授权码粘贴到下方。授权码通过 RPC 交给 CLI，换取凭据和刷新均由 CLI 完成。</p><form onSubmit={submit}><label htmlFor="auth-code">授权码</label><textarea id="auth-code" value={code} onChange={e=>setCode(e.target.value)} placeholder="code#state" autoComplete="off" spellCheck={false} rows={3} maxLength={4096}/><button className="secondary full" disabled={!!busy||!code.trim()||login!.status!=='waiting'}>{login!.status==='submitting'?'正在完成登录…':'提交授权码'}</button></form></>}<button className="text-button full" onClick={cancel} disabled={!!busy}>取消本次登录</button></div>}{login&&!running&&<div className={`login-result ${login.status==='succeeded'?'success':''}`} role="status"><strong>{labels[login.status]}</strong>{login.message&&<p>{login.message}</p>}</div>}</section>
    <section className="panel how-it-works"><div><p className="eyebrow">HOW IT CONNECTS</p><h2>凭据留在服务端</h2></div><div className="flow-diagram"><span><Icon name="grid"/>Web 控制台<small>Admin token</small></span><Icon name="arrow"/><span><Icon name="shield"/>CLI OAuth RPC<small>浏览器授权</small></span><Icon name="arrow"/><span><Icon name="terminal"/>Claude CLI<small>按请求使用凭据</small></span></div></section></div>:<div className="api-layout"><section className="panel api-panel"><h2>连接地址</h2><p className="muted">将客户端 Base URL 指向桥接服务。</p><div className="endpoint"><code>{base}</code><button className="secondary" onClick={()=>copy(base)}>复制</button></div><div className="method-row"><span>POST</span><code>/v1/messages</code><span className="pill">JSON · SSE</span></div><h3>请求示例</h3><pre>{`curl ${base}/v1/messages \\\n  -H 'content-type: application/json' \\\n  -H 'anthropic-version: 2023-06-01' \\\n${data.service.api_key_required?"  -H 'x-api-key: <BRIDGE_API_KEY>' \\\n":''}  -d '{"model":"claude-sonnet-4-6","max_tokens":256,\n       "messages":[{"role":"user","content":"你好"}]}'`}</pre></section><section className="panel config-panel"><Icon name="shield" size={26}/><h2>两类令牌，独立用途</h2><dl><div><dt>BRIDGE_ADMIN_TOKEN</dt><dd>用于本控制台和账号管理接口。</dd></div><div><dt>BRIDGE_API_KEY</dt><dd>用于应用调用 Messages API。</dd></div></dl><p className="fine-print">管理令牌不应分发给模型 API 调用方。修改服务端环境变量后重启服务生效。</p><div className="subtle-rule"/><p className="muted small">支持 Messages 核心结构，未支持参数返回 400。CLI 仍可能添加日期等上下文。</p></section></div>}
    <footer className="content-footer"><span><span className="live-dot"/>连接至 {data.service.bind}</span><span>单实例单账号 · Claude Code OAuth</span></footer></main></div></div>
}
function Logo() { return <div className="logo"><span/><span/><span/></div> }
function Metric({label,value,detail,icon,good=false}:{label:string;value:string;detail:string;icon:string;good?:boolean}) { return <div className="metric"><div className="metric-label">{label}<Icon name={icon} size={18}/></div><strong className={good?'good':''}>{value}</strong><small>{detail}</small></div> }
