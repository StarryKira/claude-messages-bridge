export type Login = {id: string; status: 'starting'|'waiting'|'submitting'|'succeeded'|'failed'|'cancelled'|'expired'; authorization_url: string|null; message: string|null; expires_at: string}
export type Status = {
  account: {binding:{account_id:string|null;email:string|null}|null; logged_in:boolean; method:string|null; provider:string|null; email:string|null; organization:string|null; subscription:string|null};
  login: Login|null;
  service: {version:string; bind:string; max_concurrency:number; available_slots:number; api_key_required:boolean; request_timeout_seconds:number; credential_mode:string; messages_path:string};
}
export class ApiError extends Error { constructor(message:string, public status:number) { super(message) } }
export async function api<T>(token:string, path:string, method='GET', body?:unknown):Promise<T> {
  const response = await fetch(`/api/admin${path}`, {
    method, headers:{Authorization:`Bearer ${token}`, ...(body===undefined?{}:{'Content-Type':'application/json'})},
    body:body===undefined?undefined:JSON.stringify(body), cache:'no-store', signal:AbortSignal.timeout(20000),
  })
  const value = await response.json().catch(() => null)
  if (!response.ok) throw new ApiError(value?.error?.message ?? `请求失败 (${response.status})`, response.status)
  return value as T
}
export const active = (login:Login|null) => !!login && ['starting','waiting','submitting'].includes(login.status)
