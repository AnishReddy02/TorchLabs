import {FormEvent, useEffect, useState} from 'react';
import './agent.css';
import './pitch.css';

type Event = {id:number;kind:string;data:Record<string,unknown>;created_at:string};
type Constraints = {target_asset?:string;max_runtime_minutes:number;stop_after_failure:boolean;software_changes_require_approval:boolean;arbitrary_shell:string};
type Session = {
  id:string;prompt:string;status:string;paused:boolean;decision:boolean|null;error?:string;
  constraints:Constraints;
  report?:{summary:string;likely_cause:string;evidence:string[];recommended_next_action:string;confidence:number};
  pending?:{request_id:string;asset:string;workflow:string;reason:string;bundle:{name:string;sha256:string;filename:string};notice:string};events:Event[]
};
const api=import.meta.env.VITE_API_URL??'http://localhost:8080';
const terminal=(status:string)=>['COMPLETED','FAILED','CANCELLED'].includes(status);
async function request<T>(path:string,body?:unknown):Promise<T>{
  const token=localStorage.getItem('bench_token')??import.meta.env.VITE_API_TOKEN??'dev-admin-token';
  const response=await fetch(`${api}/api/agent${path}`,{method:body===undefined?'GET':'POST',headers:{authorization:`Bearer ${token}`,'content-type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)});
  const data=await response.json();if(!response.ok)throw new Error(data.error??`HTTP ${response.status}`);return data;
}

export function Agent(){
  const [prompt,setPrompt]=useState('Test the latest rover-demo build and investigate any failures.');
  const [targetAsset,setTargetAsset]=useState('Rover-03');
  const [maxMinutes,setMaxMinutes]=useState(20);
  const [stopAfterFailure,setStopAfterFailure]=useState(true);
  const [instruction,setInstruction]=useState('');
  const [sessions,setSessions]=useState<Session[]>([]);
  const [id,setId]=useState<string|undefined>(()=>new URLSearchParams(location.search).get('session')??undefined);const [session,setSession]=useState<Session>();
  const [error,setError]=useState('');const [busy,setBusy]=useState(false);
  const capture=new URLSearchParams(location.search).get('capture');
  useEffect(()=>{let live=true;const load=async()=>{try{const list=await request<Session[]>('/sessions');const detail=id?await request<Session>(`/sessions/${id}`):undefined;if(live){setSessions(list);setSession(detail);}}catch(e){if(live)setError(String(e));}};void load();const timer=setInterval(load,1000);return()=>{live=false;clearInterval(timer);};},[id]);
  async function action(fn:()=>Promise<void>){setBusy(true);setError('');try{await fn();}catch(e){setError(String(e));}finally{setBusy(false);}}
  function start(e:FormEvent){e.preventDefault();void action(async()=>{const created=await request<{id:string}>('/sessions',{prompt,target_asset:targetAsset||null,max_runtime_minutes:maxMinutes,stop_after_failure:stopAfterFailure});setId(created.id);});}
  function control(name:'pause'|'resume'|'cancel'){if(!session)return;void action(async()=>{await request(`/sessions/${session.id}/${name}`,{});});}
  function steer(e:FormEvent){e.preventDefault();if(!session||!instruction.trim())return;void action(async()=>{await request(`/sessions/${session.id}/instructions`,{message:instruction});setInstruction('');});}
  const stateLabel=session?.paused?'PAUSED':session?.status.replaceAll('_',' ');
  return <section className={`agent ${capture?`pitch-${capture}`:''}`}>
    <div className="agent-heading"><div><span className="eyebrow">Human-supervised autonomy</span><h1>Torch Agent Control</h1><p>Set the mission and boundaries. Torch controls access, approvals, execution, and cleanup.</p></div><div className="control-legend"><span className="dot live"/> Operator connected</div></div>

    <div className="mission-panel">
      <div className="panel-title"><div><span className="step-number">1</span><div><h2>Define the mission</h2><small>The agent can choose only typed Torch actions.</small></div></div></div>
      <form className="mission-form" onSubmit={start}>
        <label className="task-field">Objective<textarea aria-label="Agent objective" maxLength={4000} value={prompt} onChange={e=>setPrompt(e.target.value)}/></label>
        <div className="mission-options">
          <label>Target asset<input value={targetAsset} maxLength={100} onChange={e=>setTargetAsset(e.target.value)} placeholder="Any compatible asset"/></label>
          <label>Time limit<select value={maxMinutes} onChange={e=>setMaxMinutes(Number(e.target.value))}><option value={10}>10 minutes</option><option value={20}>20 minutes</option><option value={30}>30 minutes</option><option value={60}>60 minutes</option></select></label>
          <label className="check"><input type="checkbox" checked={stopAfterFailure} onChange={e=>setStopAfterFailure(e.target.checked)}/> Stop after first failed workflow</label>
        </div>
        <button className="primary launch" disabled={busy||!prompt.trim()}>Start supervised mission</button>
      </form>
      <div className="guardrails"><div><b>Automatically allowed</b><span>Inspect · reserve · approved tests · logs · release</span></div><div><b>Human approval</b><span>Deploying or selecting a software bundle</span></div><div><b>Always blocked</b><span>Arbitrary shell · firmware · safety bypass</span></div></div>
    </div>

    {error&&<div role="alert" className="error">{error}</div>}
    <div className="agent-layout"><aside><h3>Mission history</h3>{sessions.map(item=><button className={item.id===id?'selected':''} key={item.id} onClick={()=>setId(item.id)}>{item.prompt.slice(0,58)}<small>{item.paused?'PAUSED':item.status}</small></button>)}</aside>
    {session?<article className="control-room">
      <div className="control-bar"><div><span className={`status-light ${session.paused?'paused':session.status.toLowerCase()}`}/><div><small>MISSION STATE</small><h2>{stateLabel}</h2></div></div><div className="control-actions">{!terminal(session.status)&&<>{session.paused?<button className="primary" disabled={busy} onClick={()=>control('resume')}>▶ Resume agent</button>:<button disabled={busy} onClick={()=>control('pause')}>Ⅱ Pause agent</button>}<button className="danger" disabled={busy} onClick={()=>control('cancel')}>Cancel &amp; release</button></>}</div></div>
      <div className="boundary-strip"><span><b>Asset</b>{session.constraints?.target_asset||'Any compatible'}</span><span><b>Deadline</b>{session.constraints?.max_runtime_minutes??20} min</span><span><b>Deployment</b>Approval required</span><span><b>Shell</b>Denied</span></div>
      {session.paused&&<div className="paused-notice"><b>Agent decisions are paused.</b> A workflow already executing on the node continues. Use Cancel &amp; release to stop it safely.</div>}
      {!terminal(session.status)&&<form className="steer" onSubmit={steer}><label>Steer the agent<input value={instruction} maxLength={1000} onChange={e=>setInstruction(e.target.value)} placeholder="Add an instruction for the next safe decision…"/></label><button disabled={busy||!instruction.trim()||session.status==='WAITING_FOR_APPROVAL'}>Send instruction</button><small>Applied between tool actions. Use Cancel for immediate intervention.</small></form>}
      {session.error&&<p className="error">{session.error}</p>}
      {session.status==='WAITING_FOR_APPROVAL'&&session.pending&&<div className="agent-approval"><div className="approval-head"><span>!</span><div><small>HUMAN DECISION REQUIRED</small><h3>Approve software deployment?</h3></div></div><p>Transfer <b>{session.pending.bundle?.name}</b> to <b>{session.pending.asset}</b> and execute <b>{session.pending.workflow}</b>.</p><blockquote>{session.pending.reason}</blockquote><small>{session.pending.notice}</small><p><code>SHA-256: {session.pending.bundle?.sha256}</code></p>{session.decision===null?<div className="actions"><button className="primary" disabled={busy} onClick={()=>void action(async()=>{await request(`/sessions/${session.id}/approval`,{approved:true,request_id:session.pending!.request_id});})}>Approve deployment</button><button disabled={busy} onClick={()=>void action(async()=>{await request(`/sessions/${session.id}/approval`,{approved:false,request_id:session.pending!.request_id});})}>Deny</button></div>:<p>Decision recorded. Resuming…</p>}</div>}
      <div className="timeline-heading"><h3>Live activity</h3><span>Audited by Torch</span></div>
      <ol className="agent-timeline">{session.events.map(e=><li key={e.id}><span className={`event-icon ${e.kind.toLowerCase()}`}>{eventIcon(e.kind)}</span><div><div><strong>{e.kind.replaceAll('_',' ')}</strong><time>{new Date(e.created_at).toLocaleTimeString()}</time></div><p>{String(e.data.message??(e.data.action as {tool?:string})?.tool??e.data.summary??'')}</p><details><summary>View action and evidence</summary><pre>{JSON.stringify(e.data,null,2)}</pre></details></div></li>)}</ol>
      {session.report&&<div className="agent-report"><span className="eyebrow">Agent diagnosis · grounded in run evidence</span><h3>Report</h3><p className="report-summary">{session.report.summary}</p><h4>Likely cause</h4><p>{session.report.likely_cause}</p><ul>{session.report.evidence.map((e,i)=><li key={i}>{e}</li>)}</ul><h4>Recommended next action</h4><p>{session.report.recommended_next_action}</p><small>Model confidence: {Math.round(session.report.confidence*100)}% — estimate, not a calibrated probability.</small></div>}
    </article>:<div className="control-empty"><span>◎</span><h2>No mission selected</h2><p>Start a supervised mission or select one from history.</p></div>}</div>
  </section>;
}

function eventIcon(kind:string){if(kind.includes('FAILED')||kind.includes('DENIED'))return '×';if(kind.includes('APPROVAL'))return '!';if(kind==='PAUSED')return 'Ⅱ';if(kind==='RESUMED')return '▶';if(kind==='OPERATOR_INSTRUCTION')return '→';if(kind.includes('SUCCEEDED')||kind==='COMPLETED')return '✓';return '·'}
