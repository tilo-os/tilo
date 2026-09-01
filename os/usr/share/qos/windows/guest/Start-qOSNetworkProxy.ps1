<#
The Windows half of a qOS `NetHost` lease. The actual concurrent relay is C#:
PowerShell ThreadPool scriptblocks have no reliable Runspace, so they must not
carry proxy connections. This process accepts only loopback clients, attaches
the opaque lease bearer to a CONNECT request for the host-only gateway, and
never installs a route or NAT adapter.
#>
[CmdletBinding()]
param([Parameter(Mandatory = $true)][ValidateScript({ Test-Path $_ -PathType Leaf })][string]$JobPath)
$ErrorActionPreference = 'Stop'
$job = Get-Content -Raw -LiteralPath $JobPath | ConvertFrom-Json
$HostEndpoint = [string]$job.networkProxy.endpoint; $LeaseToken = [string]$job.networkProxy.token
if ($HostEndpoint -notmatch '^192\.168\.251\.1:([1-9][0-9]{0,4})$' -or [int]$Matches[1] -gt 65535) { throw 'qOS network proxy endpoint is not on the host-only gateway' }
$hostPort = [int]$Matches[1]
if ($LeaseToken -notmatch '^[A-Fa-f0-9]{64}$') { throw 'qOS network proxy lease token is invalid' }

Add-Type -TypeDefinition @'
using System; using System.IO; using System.Net; using System.Net.Sockets; using System.Text; using System.Threading;
public static class QosNetworkProxy {
  static string Head(Stream s) { var b=new MemoryStream(); int x; while(b.Length<16384 && (x=s.ReadByte())>=0) { b.WriteByte((byte)x); var a=b.GetBuffer(); var n=(int)b.Length; if(n>=4&&a[n-4]==13&&a[n-3]==10&&a[n-2]==13&&a[n-1]==10) return Encoding.ASCII.GetString(a,0,n); } throw new IOException("invalid proxy head"); }
  static string[] Target(string h, out string forward) { var first=h.Split(new[]{"\r\n"},StringSplitOptions.None)[0].Split(' '); if(first.Length<3) throw new IOException("invalid request"); forward=null; if(String.Equals(first[0],"CONNECT",StringComparison.OrdinalIgnoreCase)) return first[1].Split(':'); if(!first[1].StartsWith("http://",StringComparison.OrdinalIgnoreCase)) throw new IOException("only HTTP proxy requests"); var rest=first[1].Substring(7); var slash=rest.IndexOf('/'); var authority=slash<0?rest:rest.Substring(0,slash); var path=slash<0?"/":rest.Substring(slash); forward=h.Replace(first[1],path); return authority.Contains(":")?authority.Split(':'):new[]{authority,"80"}; }
  static void One(object state) { var p=(object[])state; var c=(TcpClient)p[0]; try { using(c) using(var local=c.GetStream()) { var head=Head(local); string forward; var hp=Target(head,out forward); if(hp.Length!=2||String.IsNullOrWhiteSpace(hp[0])) throw new IOException("invalid authority"); int port=Int32.Parse(hp[1]); using(var up=new TcpClient("192.168.251.1",(int)p[1])) using(var remote=up.GetStream()) { var connect="CONNECT "+hp[0]+":"+port+" HTTP/1.1\r\nProxy-Authorization: Bearer "+(string)p[2]+"\r\n\r\n"; var wire=Encoding.ASCII.GetBytes(connect); remote.Write(wire,0,wire.Length); if(!Head(remote).StartsWith("HTTP/1.1 200 ")) throw new IOException("lease denied"); if(forward!=null) { wire=Encoding.ASCII.GetBytes(forward); remote.Write(wire,0,wire.Length); } else { wire=Encoding.ASCII.GetBytes("HTTP/1.1 200 Connection Established\r\n\r\n"); local.Write(wire,0,wire.Length); } var copy=new Thread(()=>{try{local.CopyTo(remote);}catch{}}); copy.IsBackground=true; copy.Start(); remote.CopyTo(local); } } } catch { try { c.Close(); } catch {} } }
  public static void Run(int port, string token) { var l=new TcpListener(IPAddress.Loopback,18080); l.Start(); while(true) { var c=l.AcceptTcpClient(); ThreadPool.QueueUserWorkItem(One,new object[]{c,port,token}); } }
}
'@
[QosNetworkProxy]::Run($hostPort, $LeaseToken)
