import {useEffect, useState} from 'react';

type Incident = {
  id:string; asset_name:string; service_id:string; status:string; severity:string;
  detected_at:string; resolved_at?:string; escalation_reason?:string;
  diagnosis?:Record<string,unknown>; proposed_remediation?:Record<string,unknown>;
  verification_result?:Record<string,unknown>; remediation_attempts:number;
  events?:{id:number;kind:string;actor:string;created_at:string;data:unknown}[];
  executions?:unknown[];
};
const api=import.meta.env.VITE_API_URL??'http://localhost:8080';
async function request<T>(path:string):Promise<T>{
  const token=localStorage.getItem('bench_token')??import.meta.env.VITE_API_TOKEN??'dev-admin-token';
  const response=await fetch(`${api}/api/incidents${path}`,{headers:{authorization:`Bearer ${token}`}});
  if(!response.ok)throw new Error(`Incident request failed: ${response.status}`);
  return response.json();
}
export function Incidents(){
  const [items,setItems]=useState<Incident[]>([]);
  const [id,setId]=useState<string>();
  const [detail,setDetail]=useState<Incident>();
  const [error,setError]=useState('');
  async function cancel(){
    if(!detail)return;
    const token=localStorage.getItem('bench_token')??import.meta.env.VITE_API_TOKEN??'dev-admin-token';
    const response=await fetch(`${api}/api/incidents/${detail.id}/cancel`,{method:'POST',headers:{authorization:`Bearer ${token}`}});
    if(!response.ok)setError('Cancellation could not be applied. Refresh incident status.');
  }
  useEffect(()=>{
    let live=true;
    async function load(){try{
      const list=await request<Incident[]>('');
      const selected=id?await request<Incident>(`/${id}`):undefined;
      if(live){setItems(list);setDetail(selected);setError('');}
    }catch(e){if(live)setError(String(e));}}
    void load();const timer=setInterval(load,2000);
    return()=>{live=false;clearInterval(timer);};
  },[id]);
  return <section><h1>Autonomous Recovery</h1><p>Observed failures, authorized repairs, and verified outcomes.</p>
    {error&&<p role="alert" className="error">{error}</p>}
    <div className="table">{items.map(item=><button className="run" key={item.id} onClick={()=>setId(item.id)}>
      <code>{item.id.slice(0,8)}</code><span>{item.asset_name}</span><span>{item.service_id}</span>
      <span className="pill">{item.status}</span><span>{new Date(item.detected_at).toLocaleString()}</span>
    </button>)}</div>
    {!items.length&&<p>No incidents recorded. Monitoring is opt-in for the configured test service.</p>}
    {detail&&<article style={{marginTop:24}}><h2>{detail.asset_name} / {detail.service_id}</h2>
      <p>{detail.severity} · {detail.status} · {detail.remediation_attempts} repair attempt(s)</p>
      {!['RESOLVED','ESCALATED','FAILED','CANCELLED'].includes(detail.status)&&<p><button onClick={()=>void cancel()}>Cancel investigation</button> An already accepted restart may finish; cancellation does not guarantee a physical stop.</p>}
      {detail.escalation_reason&&<p className="error">Escalated: {detail.escalation_reason}</p>}
      <h3>Assessment · {String(detail.diagnosis?.source??'pending')}</h3><pre>{JSON.stringify(detail.diagnosis,null,2)}</pre>
      <h3>Proposed repair</h3><pre>{JSON.stringify(detail.proposed_remediation,null,2)}</pre>
      <h3>Policy and actual execution</h3><pre style={{whiteSpace:'pre-wrap'}}>{JSON.stringify(detail.executions,null,2)}</pre>
      <h3>Observed recovery verification</h3><pre>{JSON.stringify(detail.verification_result,null,2)}</pre>
      {detail.resolved_at&&<p>Time from detection to verified recovery: {((Date.parse(detail.resolved_at)-Date.parse(detail.detected_at))/1000).toFixed(1)} seconds.</p>}
      <h3>Incident timeline</h3><ol>{detail.events?.map(event=><li key={event.id}>
        <strong>{event.kind}</strong> · {event.actor} · {new Date(event.created_at).toLocaleTimeString()}
        <details><summary>Evidence and decision</summary><pre style={{whiteSpace:'pre-wrap'}}>{JSON.stringify(event.data,null,2)}</pre></details>
      </li>)}</ol>
    </article>}
  </section>;
}
