<script>
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { getCurrentWebview } from '@tauri-apps/api/webview';
  import { startDrag } from '@crabnebula/tauri-plugin-drag';
  let archive = $state(null), selected = $state(''), status = $state('准备就绪'), busy = $state(false), dark = $state(false), modal = $state('');
  let destination = $state(''), folder = $state(''), extra = $state(false), item = $state('');
  let format = $state('zip'), name = $state(''), level = $state(6), progress = $state({fraction:0,text:'正在检查压缩包…',remaining:'预计剩余时间：计算中…'});
  let pointer = null, openPending = $state(false);
  let scrollTop = $state(0), listHeight = $state(500), rowsElement = $state();
  let start = $derived(Math.max(0, Math.floor(scrollTop / 48) - 8));
  let end = $derived(Math.min(archive?.entries.length || 0, Math.ceil((scrollTop + listHeight) / 48) + 8));
  let visibleRows = $derived(archive?.entries.slice(start, end) || []);
  $effect(()=>{if(openPending && !busy && !modal) {setTimeout(drainOpen,0);}});
  async function drainOpen() { if (busy || modal) {openPending=true; return;} openPending=false; const path=await invoke('take_open'); if(path) {apply(await task('正在打开压缩包…',()=>invoke('open_archive',{path}),'压缩包已打开')); await drainOpen();} }
  const icons = {folder:'▰',image:'图',audio:'♫',video:'▶',archive:'▣',code:'</>',document:'文',file:'▤'};
  async function task(label, fn, success) { if (busy) return; busy=true; status=label; try { const result=await fn(); if(success) status=success; else status='准备就绪'; return result; } catch(e) { status='错误：'+e; } finally {busy=false; if(openPending) setTimeout(drainOpen,0);} }
  function apply(v) { if(v) {archive=v; selected=''; scrollTop=0; if(rowsElement) rowsElement.scrollTop=0;} }
  async function openArchive() {apply(await task('正在打开压缩包…',()=>invoke('open_archive',{path:null}),'压缩包已打开'));}
  async function newArchive() {apply(await task('正在创建压缩包…',()=>invoke('create_empty'),'压缩包已创建'));}
  async function navigate(directory) {if(!archive||busy)return; apply(await task('正在读取目录…',()=>invoke('navigate',{directory})));}
  async function add() {apply(await task('正在添加文件…',()=>invoke('add',{directory:archive.directory}),'文件已添加'));}
  function compressDialog() { format='zip';level=6;name=archive.folder||'新建压缩包';destination=archive.parent;modal='compress'; }
  function extractDialog(path='') {item=path;destination=archive.parent;folder=archive.folder;extra=false;modal='extract';}
  async function chooseFolder() {const p=await invoke('choose_folder');if(p)destination=p;}
  async function compress() {modal='';apply(await task('正在创建压缩包…',()=>invoke('compress',{destination,name,format,level}),'压缩包已创建'));}
  async function extract() {modal='progress';progress={fraction:0,text:'正在检查压缩包…',remaining:'预计剩余时间：计算中…'}; await task('正在解压文件…',()=>invoke('extract',{destination,item,extra:extra?folder:null}),'解压完成');modal='';}
  async function activate(row) {if(row.directory) await navigate(row.path); else if(!archive.staging) await task('正在打开文件…',()=>invoke('open_item',{item:row.path}),'文件已打开');}
  function down(e,row) { if(e.button!==0 || busy || archive.staging)return; pointer={x:e.clientX,y:e.clientY,row,active:true,started:false};selected=row.path; }
  function up() {if(pointer)pointer.active=false;}
  async function move(e) {let p=pointer;if(!p||!p.active||p.started||Math.hypot(e.clientX-p.x,e.clientY-p.y)<8)return;p.started=true;status='正在准备拖出内容，请按住鼠标…';try {let path=await invoke('prepare_drag',{item:p.row.path});if(p.active) {await startDrag({item:[path],icon:dragIcon,mode:'copy'},({result})=>status=result==='Dropped'?'已交给文件管理器复制到目标文件夹':'已取消拖出解压');} else status='内容已准备，请再次拖动';} catch(e){status='错误：'+e;} }
  const dragIcon='data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z/D/PwAHggJ/PchS3wAAAABJRU5ErkJggg==';
  onMount(()=> {let un=[]; (async()=> {un.push(await listen('open-files',()=>drainOpen()));un.push(await listen('extract-progress', e=>progress=e.payload));un.push(await getCurrentWebview().onDragDropEvent(async e=>{if(e.payload.type==='drop'&&!busy&&!modal){apply(await task('正在读取拖入的文件…',()=>invoke('stage',{paths:e.payload.paths}),'文件已加入待压缩列表'));}}));let v=await task('正在启动…',()=>invoke('startup'));if(v){apply(v);if(v.staging)compressDialog();} await drainOpen();})().catch(e=>status='Error: '+e); return ()=>un.forEach(f=>f());});
</script>
<svelte:window onpointerup={up} onpointermove={move}/>
<div class:dark class="app">
  <div class="menubar"><button class="menubutton" onclick={()=>modal='appearance'} disabled={busy}>设置</button></div>
  <div class="layout">
    <aside><div class="brand"><div class="logo">S</div><div><strong>SUGAR</strong><small>ARCHIVE</small></div></div><div class="side-label">{archive?.staging?'待压缩目录':'压缩包目录'}</div>
      {#if archive}<button class:current={archive.directory===''} class="tree" onclick={()=>navigate('')}>▣　{archive.name}</button><div class="tree-list">{#each archive.tree as dir}<button class:current={archive.directory===dir.path} class="tree" style:padding-left={`${10+dir.depth*14}px`} onclick={()=>navigate(dir.path)}>▸　{dir.name}</button>{/each}</div>{:else}<p class="muted">打开压缩包后显示目录</p>{/if}
    </aside>
    <main><div class="toolbar"><button onclick={openArchive} disabled={busy}>↗ &nbsp;打开文件</button><button onclick={newArchive} disabled={busy}>＋ &nbsp;新建</button><span class="divider"></span><button onclick={add} disabled={busy||!archive?.can_add}>＋ &nbsp;添加文件</button>{#if archive?.staging}<button class="primary" onclick={compressDialog} disabled={busy}>压缩</button>{:else}<button class="primary" onclick={()=>extractDialog()} disabled={busy||!archive}>↓ &nbsp;解压全部</button><button onclick={()=>extractDialog(selected)} disabled={busy||!selected}>↓ &nbsp;解压所选</button>{/if}</div>
      <section class="content"><header><button onclick={()=>navigate(archive.directory.replace(/[^/]+\/$/,''))} disabled={!archive||!archive.directory||busy}>← 返回</button><b>{archive?'根目录 / '+archive.directory:'压缩包内容'}</b><span class="muted">{archive?archive.entries.length+' 个项目':''}</span></header><div class="columns"><span>名称</span><span>类型</span><span>大小</span><span>压缩后</span></div>
        {#if !archive || archive.entries.length===0}<div class="empty"><div>▤</div><b>{archive?'此文件夹为空':'开始管理你的文件'}</b><p>{archive?'此目录没有内容':'打开压缩包，或拖入文件创建压缩包'}</p></div>{:else}<div class="rows" bind:clientHeight={listHeight} bind:this={rowsElement} onscroll={e=>scrollTop=e.currentTarget.scrollTop}><div style:height={`${start*48}px`}></div>{#each visibleRows as row (row.path)}<div role="button" tabindex="0" class:selected={selected===row.path} class="row" onpointerdown={e=>down(e,row)} onclick={()=>selected=row.path} ondblclick={()=>activate(row)} onkeydown={e=>{if(e.key==='Enter')activate(row)}}><span class="row-name"><span class="file-icon {row.icon}">{icons[row.icon]}</span><span class="ellipsis">{row.name}</span></span><span>{row.kind}</span><span>{row.size}</span><span>{row.compressed}</span></div>{/each}<div style:height={`${(archive.entries.length-end)*48}px`}></div></div>{/if}
      </section><footer><span>● &nbsp;{status}</span><span>{archive&&!archive.staging?'拖出文件或文件夹到目标文件夹解压':'ARCHIVE MANAGER'}</span></footer>
    </main>
  </div>
  {#if modal}<div class="overlay"><div class="dialog" role="dialog" aria-modal="true">
    {#if modal==='appearance'}<h2>设置</h2><b>外观</b><p>选择应用的颜色模式</p><div class="choice"><button class:primary={!dark} onclick={()=>dark=false}> {dark?'':'✓ '}亮色</button><button class:primary={dark} onclick={()=>dark=true}>{dark?'✓ ':''}暗色</button></div><div class="actions"><button onclick={()=>modal=''}>关闭</button></div>
    {:else if modal==='compress'}<h2>压缩选项</h2><b>压缩格式</b><div class="choice">{#each ['zip','7z','tar','tar.gz','tar.bz2','tar.xz'] as f}<button class:primary={format===f} onclick={()=>{format=f;if(level===0&&!['zip','7z'].includes(f))level=6}}>{f}</button>{/each}</div><label for="name">压缩包名称</label><input id="name" bind:value={name}><label for="destination">保存位置</label><div class="input-row"><input id="destination" bind:value={destination}><button onclick={chooseFolder}>浏览…</button></div><b>压缩程度</b><div class="choice">{#each [[0,'仅存储'],[1,'快速'],[6,'标准'],[9,'较小']] as option}<button disabled={option[0]===0&&!['zip','7z'].includes(format)} class:primary={level===option[0]} onclick={()=>level=option[0]}>{option[1]}</button>{/each}</div><p>{format==='tar'?'TAR 不压缩数据':'较高压缩程度可能需要更长时间'}</p><div class="actions"><button onclick={()=>modal=''}>取消</button><button class="primary" disabled={!name||!destination} onclick={compress}>开始压缩</button></div>
    {:else if modal==='extract'}<h2>解压到</h2><p>{item?'解压所选：'+item:'解压整个压缩包'}</p><label for="extract-dest">目标文件夹</label><div class="input-row"><input id="extract-dest" bind:value={destination}><button onclick={chooseFolder}>浏览…</button></div><label class="checkbox"><input type="checkbox" bind:checked={extra}>创建额外文件夹</label><div class="input-row"><label for="folder">文件夹名称</label><input id="folder" disabled={!extra} bind:value={folder}></div><div class="actions"><button onclick={()=>modal=''}>取消</button><button class="primary" disabled={!destination||(extra&&!folder)} onclick={extract}>开始解压</button></div>
    {:else}<h2>正在解压</h2><p>{progress.text}</p><div class="progress"><div style:width={`${progress.fraction*100}%`}></div></div><p>{progress.remaining}</p>{/if}
  </div></div>{/if}
</div>

