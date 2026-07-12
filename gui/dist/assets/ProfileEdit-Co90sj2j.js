import{d as x,u as B,B as E,r as s,o as S,C as b,e as m,f as r,g as y,t as g,k,j as i,h as M,i as N,m as f,w as P,n as D,l as F,F as A,D as L,E as R}from"./index-BxPSGBaa.js";const U={class:"page-header"},j={key:0,class:"empty-state"},I={key:1,class:"stack"},$={class:"form-actions"},q=`instance_name = "my-network"
dhcp = true

[network_identity]
network_name = "my-network"
network_secret = "mysecret"

[[peer]]
uri = "tcp://public.easytier.cn:11010"

[flags]
no_tun = true
`,H=x({__name:"ProfileEdit",setup(z){const h=E(),_=F(),l=B(),o=h.params.id,a=s(""),t=s(null),u=s(!1),c=s(!1),d=s(!1);S(async()=>{if(o){d.value=!0;try{const e=await b(o);a.value=e.toml}catch(e){l.add({severity:"error",summary:"加载配置失败",detail:String(e),life:5e3})}finally{d.value=!1}}else a.value=q});async function w(){u.value=!0,t.value=null;try{return await L(a.value),l.add({severity:"success",summary:"配置校验通过",life:3e3}),!0}catch(e){return t.value=String(e),l.add({severity:"error",summary:"配置校验失败",detail:String(e),life:5e3}),!1}finally{u.value=!1}}async function C(){c.value=!0;try{await R(o??null,a.value),_.push("/")}catch(e){t.value=String(e),l.add({severity:"error",summary:"保存失败",detail:String(e),life:5e3})}finally{c.value=!1}}return(e,n)=>{const T=f("Textarea"),V=f("Message"),v=f("Button");return r(),m(A,null,[y("div",U,[y("h1",null,g(k(o)?"编辑网络":"新建网络"),1)]),d.value?(r(),m("div",j,"加载中…")):(r(),m("div",I,[i(T,{modelValue:a.value,"onUpdate:modelValue":n[0]||(n[0]=p=>a.value=p),rows:"20",class:"mono-textarea",style:{width:"100%"}},null,8,["modelValue"]),t.value?(r(),M(V,{key:0,severity:"error",closable:!1},{default:P(()=>[D(g(t.value),1)]),_:1})):N("",!0),y("div",$,[i(v,{label:"校验",severity:"secondary",loading:u.value,onClick:w},null,8,["loading"]),i(v,{label:"保存",loading:c.value,onClick:C},null,8,["loading"]),i(v,{label:"取消",severity:"secondary",outlined:"",onClick:n[1]||(n[1]=p=>k(_).push("/"))})])]))],64)}}});export{H as default};
