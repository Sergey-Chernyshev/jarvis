/* Session-bound file previews. Artifact contents are data, never app markup. */
(() => {
  const el = (tag, cls, text) => { const n = document.createElement(tag); n.className = cls || ''; if (text != null) n.textContent = text; return n; };
  const button = (label, action, cls = '') => { const b = el('button', cls, label); b.type = 'button'; b.addEventListener('click', action); return b; };
  const kindOf = (name, type = '') => {
    const ext = name.split('.').pop().toLowerCase();
    if (type.startsWith('image/') || /^(png|jpe?g|gif|webp|svg|avif|bmp)$/.test(ext)) return 'image';
    if (ext === 'pdf' || type === 'application/pdf') return 'pdf';
    if (/^(html?|xhtml)$/.test(ext)) return 'html';
    if (/^(mp4|webm|mov)$/.test(ext)) return 'video';
    if (/^(mp3|wav|ogg|m4a)$/.test(ext)) return 'audio';
    if (/^(docx?|rtf)$/.test(ext)) return 'document';
    return 'text';
  };
  const filePath = value => {
    let p = String(value || '').trim();
    if (p.startsWith('sandbox:')) p = p.slice(8);
    if (p.startsWith('file://')) { try { p = decodeURI(p.slice(7)); } catch { return null; } }
    if (!p || p.length > 4096 || /[\x00-\x1f]/.test(p) || /^[a-z][a-z\d+.-]*:/i.test(p) || p.startsWith('//')) return null;
    return p;
  };
  function references(text) {
    const refs = [], seen = new Set();
    const add = (value, label) => { const path = filePath(value); if (path && !seen.has(path)) { seen.add(path); refs.push({path,name:label || path.split('/').pop()}); } };
    for (const match of String(text).matchAll(/!?\[([^\]\n]*)\]\((<[^>]+>|(?:[^()\s]|\([^()]*\))+)\)/g)) add(match[2].replace(/^<|>$/g,''), match[1]);
    for (const match of String(text).matchAll(/`((?:\.?\.?\/|\/)[^`\n]+\.[a-zA-Z\d]{1,10})`/g)) add(match[1]);
    for (const match of String(text).matchAll(/(?:^|\n)"((?:\/|[a-zA-Z]:)[^\n]+)"(?=\n|$)/g)) { try { add(JSON.parse('"'+match[1]+'"')); } catch {} }
    return refs;
  }
  function safeHtml(content){
    const doc=new DOMParser().parseFromString(content,'text/html');
    doc.querySelectorAll('script,meta,base,iframe,object,embed,link').forEach(n=>n.remove());
    for(const node of doc.querySelectorAll('*'))for(const attr of [...node.attributes])if(/^on/i.test(attr.name)||['href','action','formaction','srcdoc'].includes(attr.name))node.removeAttribute(attr.name);
    return doc.documentElement.outerHTML;
  }
  const root = el('aside','artifact-viewer'); root.id = 'artifactViewer'; root.hidden = true; root.setAttribute('aria-label','Просмотр файла');
  const head = el('header','artifact-head'), title = el('strong'), source = el('small'); const names = el('div'); names.append(title,source);
  const closeButton = button('Закрыть', close, 'artifact-close'); head.append(names,closeButton);
  const toolbar = el('div','artifact-toolbar'), modes = el('div','artifact-modes'), zoom = el('select'); zoom.setAttribute('aria-label','Масштаб изображения');
  for (const [value,label] of [['fit','По размеру'],['1','100%'],['1.5','150%'],['2','200%']]) { const option=el('option','',label); option.value=value; zoom.append(option); }
  const copyPath = button('Скопировать путь', async () => { if (current?.path) { try { const r=await window.jarvis.copyText(current.path); if(r?.ok===false)throw Error(r.error); status.textContent='Путь скопирован'; } catch(e){status.textContent=e.message;} } });
  toolbar.append(modes,zoom,copyPath);
  const body = el('div','artifact-body'), preview = el('div','artifact-preview'), rail = el('section','artifact-notes'); rail.setAttribute('aria-label','Комментарии к файлу');
  const notesTitle=el('strong','','Комментарии'), notesHelp=el('p','artifact-hint','Сохраняются на этом компьютере для этой версии файла.'), notesList=el('div','artifact-note-list');
  const form=el('form','artifact-note-form'), anchorLabel=el('div','artifact-anchor'), input=el('textarea'); input.placeholder='Добавить комментарий…'; input.setAttribute('aria-label','Комментарий к файлу'); input.maxLength=12000;
  const clearAnchor=button('Убрать привязку',()=>{anchor=null;paintAnchor();}), submit=el('button','','Сохранить'); submit.type='submit';
  const status=el('div','artifact-status'); status.setAttribute('role','status'); form.append(anchorLabel,clearAnchor,input,submit,status);
  const hint=el('p','artifact-hint','Выдели текст или отметь точку на фотографии, чтобы привязать комментарий.'); rail.append(notesTitle,notesHelp,notesList,hint,form); body.append(preview,rail); root.append(head,toolbar,body); document.body.append(root);
  const drafts = new Map();
  function rememberDraft(){if(payload?.key)drafts.set(payload.key,{text:input.value,anchor,editing});}
  let sequence=0, current=null, payload=null, url=null, notes=[], anchor=null, editing=null, mode='preview', opener=null, config={};
  function close() { if(root.hidden)return; rememberDraft(); sequence++; root.hidden=true; document.documentElement.classList.remove('artifact-open'); if(url)URL.revokeObjectURL(url);url=null;opener?.focus?.(); }
  function paintAnchor(){anchorLabel.textContent=anchor?.quote ? '«'+anchor.quote+'»' : anchor?.x != null ? `Точка ${Math.round(anchor.x*100)}%, ${Math.round(anchor.y*100)}%` : 'Весь файл'; clearAnchor.hidden=!anchor;}
  function setAnchor(value){anchor=value;paintAnchor();input.focus();}
  function paintNotes(){
    notesList.replaceChildren();
    if(!notes.length)notesList.append(el('p','artifact-hint','Здесь будут твои заметки.'));
    for(const note of notes){
      const card=el('article','artifact-note'); card.dataset.noteId=note.id;
      if(note.anchor?.quote)card.append(el('blockquote','',note.anchor.quote));
      else if(note.anchor?.x != null)card.append(button('Показать точку',()=>{ const pin=preview.querySelector(`[data-pin="${note.id}"]`); pin?.scrollIntoView?.({block:'center'}); pin?.focus(); }));
      card.append(el('p','',note.text)); const actions=el('div','artifact-note-actions');
      actions.append(button('Изменить',()=>{editing=note.id;input.value=note.text;setAnchor(note.anchor);submit.textContent='Сохранить изменения';}),button('Удалить',()=>saveNote(null,note.id)));
      if(current.sessionId)actions.append(button('В чат',()=>{const target={...current}; const text=`Комментарий к файлу ${target.path || target.name}:\n${note.anchor?.quote ? '«'+note.anchor.quote+'»\n' : note.anchor?.x != null ? `Точка на изображении: ${Math.round(note.anchor.x*100)}%, ${Math.round(note.anchor.y*100)}%\n` : ''}${note.text}`;close();config.comment?.(target.sessionId,text);}));
      card.append(actions);notesList.append(card);
    }
    paintPins();
  }
  function paintPins(){ const stage=preview.querySelector('.artifact-image-stage'); if(!stage)return; stage.querySelectorAll('.artifact-pin').forEach(n=>n.remove()); notes.forEach((note,i)=>{if(note.anchor?.x==null)return; const pin=button(String(i+1),e=>{e.stopPropagation();notesList.querySelector(`[data-note-id="${note.id}"]`)?.scrollIntoView?.({block:'nearest'});},'artifact-pin');pin.dataset.pin=note.id;pin.style.left=(Math.max(0,Math.min(1,note.anchor.x))*100)+'%';pin.style.top=(Math.max(0,Math.min(1,note.anchor.y))*100)+'%';pin.title=note.text; stage.append(pin);}); }
  async function saveNote(note, removeId=null){
    const request=sequence, key=payload?.key; if(!key)return; submit.disabled=true;status.textContent='Сохраняем…';
    try {const result=await window.jarvis.artifactNotes(key,note,removeId);if(!result?.ok)throw Error(result?.error||'Не удалось сохранить комментарий');if(request!==sequence)return;notes=result.notes;paintNotes();if(note){drafts.delete(key);input.value='';editing=null;anchor=null;paintAnchor();submit.textContent='Сохранить';}status.textContent='Сохранено';}
    catch(error){if(request===sequence)status.textContent=error.message;}
    finally{if(request===sequence)submit.disabled=false;}
  }
  form.addEventListener('submit',event=>{event.preventDefault();if(input.value.trim())saveNote({id:editing||crypto.randomUUID(),text:input.value.trim(),anchor});});
  preview.addEventListener('mouseup',()=>{const selection=window.getSelection();const text=selection?.toString().trim();if(text && preview.contains(selection.anchorNode) && preview.contains(selection.focusNode)) {anchor={quote:text.slice(0,2000)};paintAnchor();}});
  function render(){
    preview.replaceChildren();modes.replaceChildren();const kind=kindOf(payload.name,payload.type);zoom.hidden=kind!=='image';copyPath.hidden=!current.path;
    if(current.changes)modes.append(button('Изменения',()=>{const action=current.changes;close();action();}));
    const content=['image','pdf','video','audio'].includes(kind) ? '' : payload.content ?? new TextDecoder().decode(payload.bytes);
    const showSource=kind==='html'||/\.(md|markdown)$/i.test(payload.name);
    if(showSource)for(const [value,label]of[['preview','Просмотр'],['source','Исходник']]){const b=button(label,()=>{mode=value;render();});b.setAttribute('aria-pressed',String(mode===value));modes.append(b);}
    if(kind==='image'){
      const stage=el('div','artifact-image-stage'),img=el('img');img.src=url;img.alt=payload.name;img.draggable=false;
      img.onload=()=>{if(zoom.value!=='fit')img.style.width=(img.naturalWidth*Number(zoom.value))+'px';};
      img.onerror=()=>{preview.replaceChildren(window.JarvisAsyncState.message({title:'Не удалось показать изображение',detail:'Формат не поддерживается или файл повреждён.'}));};
      if(zoom.value==='fit')stage.classList.add('fit');else img.style.width=(Number(zoom.value)*100)+'%';
      stage.append(img);img.addEventListener('click',e=>{const box=img.getBoundingClientRect();setAnchor({x:(e.clientX-box.left)/box.width,y:(e.clientY-box.top)/box.height});});preview.append(stage);paintPins();
    }else if(kind==='pdf'){
      const iframe=el('iframe','artifact-document');iframe.title=payload.name;iframe.src=url;preview.append(iframe);
      hint.textContent='К документу можно добавить общий комментарий. Номер страницы можно указать в тексте заметки.';
    }else if(kind==='html'&&mode==='preview'){
      const iframe=el('iframe','artifact-document');iframe.title=payload.name;iframe.setAttribute('sandbox','');iframe.referrerPolicy='no-referrer';
      iframe.srcdoc=`<!doctype html><meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; connect-src 'none'; form-action 'none'; base-uri 'none'"><meta name="viewport" content="width=device-width, initial-scale=1">`+safeHtml(content);preview.append(iframe);
    }else if(kind==='video'||kind==='audio'){
      const media=el(kind,'artifact-media');media.controls=true;media.src=url;preview.append(media);
    }else if(kind==='document'&&!payload.converted){preview.append(window.JarvisAsyncState.message({title:'Предпросмотр этого документа недоступен',detail:'Для просмотра содержимого сохрани документ как PDF или текст.'}));
    }else if(payload.bytes.slice(0,8000).includes(0)&&!payload.converted){preview.append(window.JarvisAsyncState.message({title:'Для этого формата пока нет предпросмотра',detail:'К файлу можно сохранить комментарии.'}));
    }else{
      const document=el('div','artifact-text');
      if(/\.(md|markdown)$/i.test(payload.name)&&mode==='preview')document.innerHTML=window.JarvisMarkdown.render(content);
      else document.append(el('pre','',content));
      if(payload.converted)document.prepend(el('p','artifact-hint','Текст документа. Исходное оформление не отображается.'));
      preview.append(document);
    }
  }
  zoom.addEventListener('change',render);
  preview.addEventListener('click',event=>{const link=event.target.closest('[data-href]');if(link){event.preventDefault();window.jarvis.openUrl(link.dataset.href);}});
  async function open(options){
    rememberDraft();const request=++sequence;opener=document.activeElement;if(url)URL.revokeObjectURL(url);url=null;current={...options};payload=null;notes=[];anchor=null;editing=null;input.value='';submit.textContent='Сохранить';submit.disabled=true;status.textContent='';mode='preview';zoom.value='fit';paintAnchor();notesList.replaceChildren();toolbar.hidden=true;
    title.textContent=options.name||options.path?.split('/').pop()||'Файл';source.textContent=options.source||'';root.hidden=false;document.documentElement.classList.add('artifact-open');closeButton.focus();preview.replaceChildren(window.JarvisAsyncState.skeleton('history','Загружаем файл…'));
    hint.textContent='Выдели текст или отметь точку на фотографии, чтобы привязать комментарий.';
    try{
      let result;
      if(options.dataUrl){
        result={ok:true,name:options.name,type:options.type,dataBase64:options.dataUrl.split(',')[1]};
        if(kindOf(options.name,options.type)==='document'){
          const saved=await window.jarvis.saveAttachment(result.dataBase64,options.name,'local');if(!saved?.ok)throw Error(saved?.error||'Не удалось подготовить документ');
          result=await window.jarvis.readArtifact(null,saved.path);
        }
      }else result=await window.jarvis.readArtifact(options.sessionId||null,options.path);
      if(request!==sequence)return;if(!result?.ok)throw Error(result?.error||'Не удалось прочитать файл');
      const binary=atob(result.dataBase64),bytes=new Uint8Array(binary.length);for(let i=0;i<binary.length;i++)bytes[i]=binary.charCodeAt(i);
      const key=result.key||'sha256:'+Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',bytes)),b=>b.toString(16).padStart(2,'0')).join('');
      if(request!==sequence)return;payload={...result,bytes,key};
      const previewType=result.type||options.type||({pdf:'application/pdf',png:'image/png',jpg:'image/jpeg',jpeg:'image/jpeg',svg:'image/svg+xml',gif:'image/gif',webp:'image/webp'}[result.name.split('.').pop().toLowerCase()])||'application/octet-stream';
      const blob=new Blob([bytes],{type:previewType});url=URL.createObjectURL(blob);
      title.textContent=result.name;source.textContent=[options.source||'',bytes.length<1024?bytes.length+' Б':bytes.length<1048576?Math.round(bytes.length/1024)+' КБ':(bytes.length/1048576).toFixed(1)+' МБ'].filter(Boolean).join(' · ');toolbar.hidden=false;render();
      const response=await window.jarvis.artifactNotes(key);if(request!==sequence)return;if(!response?.ok)throw Error(response?.error||'Не удалось загрузить комментарии');notes=response.notes;paintNotes();const draft=drafts.get(key);if(draft){input.value=draft.text;anchor=draft.anchor;editing=draft.editing;paintAnchor();submit.textContent=editing?'Сохранить изменения':'Сохранить';}submit.disabled=false;
    }catch(error){if(request!==sequence)return;const retry=()=>open(options);if(payload){status.replaceChildren(window.JarvisAsyncState.message({title:'Комментарии недоступны',detail:error.message,kind:'error',action:'Повторить',onAction:retry}));}else preview.replaceChildren(window.JarvisAsyncState.message({title:'Не удалось открыть файл',detail:error.message,kind:'error',action:'Повторить',onAction:retry}));}
  }
  // Capture Escape before the chat's navigation handler. Only the viewer closes.
  window.addEventListener('keydown',event=>{if(root.hidden)return;if(event.key==='Escape'){event.preventDefault();event.stopImmediatePropagation();close();}},true);
  function chips(root, refs, sessionId, source){if(!refs.length)return;const list=el('div','artifact-links');for(const ref of refs){const b=button(ref.name||ref.path.split('/').pop(),()=>open({...ref,sessionId,source}),'artifact-file-link');b.title=ref.path||ref.name;list.append(b);}root.append(list);}
  window.JarvisArtifacts={open,close,chips,references,filePath,kindOf,configure:options=>{config=options;}};
})();
