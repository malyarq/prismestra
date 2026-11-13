'use strict';
const $ = (selector, root = document) => root.querySelector(selector);
const esc = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const clone = value => structuredClone(value);
const same = (a,b) => JSON.stringify(a) === JSON.stringify(b);
const hex = color => '#' + (Number(color) >>> 0).toString(16).padStart(6,'0').slice(-6);
const effects = {noise_map:'Карта шума',breathing:'Дыхание',rainbow_wave:'Радужная волна'};
const pages = {library:'Сцены',editor:'Редактор',devices:'Устройства',lcd:'LCD',ambient:'Экран',temperatures:'Температуры',connections:'Подключения',sources:'Источники',schedule:'Расписание'};
const glyphs = {library:'<rect x="3" y="3" width="7" height="7"/><rect x="14" y="3" width="7" height="7"/><rect x="3" y="14" width="7" height="7"/><rect x="14" y="14" width="7" height="7"/>',editor:'<path d="M7 3v6m0 4v8M17 3v11m0 4v3M4 9h6v4H4zm10 5h6v4h-6z"/>',devices:'<rect x="3" y="4" width="18" height="13" rx="2"/><path d="M8 21h8m-4-4v4"/>',edit:'<path d="m15 4 5 5M4 20l5-1L21 7a2 2 0 0 0-5-5L4 14Z"/>',play:'<path d="m9 5 11 7-11 7Z"/>',trash:'<path d="M4 7h16M9 7V4h6v3M6 7l1 14h10l1-14M10 11v6m4-6v6"/>',plus:'<path d="M12 5v14M5 12h14"/>',sun:'<circle cx="12" cy="12" r="4"/><path d="M12 2v2m0 16v2M2 12h2m16 0h2"/>'};
const icon = name => `<svg viewBox="0 0 24 24" aria-hidden="true">${glyphs[name] || glyphs.devices}</svg>`;
let state = null, draft = null, draftName = 'Текущая сцена', selectedId = null;
let view = 'library', search = '', previewTimer = null, frame = null, commandPending = false, error = '', dialogMode = 'new', dialogReturn = null, savedSceneRequest = null, previewQueued = false, closing = false, nativeAwaitingAck = false, pendingView = null, advancedOpen = false, submittedStages = [], pointerGesture = false, keyGesture = false, composing = false, renderQueued = false, awaitingCommandAck = false, errorIsCommand = false;
const formStages = new Map();
const busy = () => closing || !state || !!state.pending || commandPending;
const disabled = () => busy() ? 'disabled' : '';
const changed = () => !!draft && !!state && !same(draft,state.scene);
function focusKey(node=document.activeElement) {
 if(!node) return null;
 if(node.id) return '#'+CSS.escape(node.id);
 for(const key of ['view','action','edit','apply','remove','field','param','color','target','group','effect','asset','device-index','remove-color','native-field','native-action']) if(node.dataset?.[key] !== undefined) return `[data-${key}="${CSS.escape(node.dataset[key])}"]`;
 return null;
}
function restoreFocus(key, selection) {
 const node = key && $(key);
 if(node && !node.disabled) {node.focus({preventScroll:true}); if(selection && node.setSelectionRange) try{node.setSelectionRange(...selection);}catch{}}
 else if(key) focusHeading();
}
function focusHeading() {const heading=$('h1');if(heading){heading.tabIndex=-1;heading.focus({preventScroll:true});}}
const recoveryOperation = op => ['revert_document','retry_document','classic'].includes(op);
const recoveryAction = action => ['revert-document','retry-document','classic'].includes(action);
function controlDisabled(node) {
 if(closing)return true;
 const code=node.dataset.action;
 if(code==='classic')return !state;
 if(code==='revert-document')return !state || !(busy()||state.editor?.dirty||stagedDirty());
 if(code==='retry-document')return !state || !(busy()||error||state.error);
 return busy() || (node.dataset.needsChange==='scene'&&!changed()) || (node.dataset.needsChange==='document'&&!(state?.editor?.dirty||currentGenericStages().length)) || (code==='add-color'&&draft?.colors.length>=8) || (node.dataset.removeColor!==undefined&&draft?.colors.length<=1);
}
const advancedPage = () => ['connections','sources','schedule'].includes(view);
function nativeFormForView() {return advancedPage()?state?.nativeForm:state?.nativeAux;}
function stageKey(kind,key) {return JSON.stringify(kind==='generic'?[kind,state?.editor?.page,state?.editor?.index,key]:[kind,view,String(key)]);}
function genericValue(field) {return formStages.get(stageKey('generic',field.key))?.value??String(field.value??'');}
function nativeValue(field) {return formStages.get(stageKey('native',field.id))?.value??String(field.value??'');}
function currentGenericStages() {return (state?.editor?.page===view?state.editor.fields:[]).filter(field=>!field.choices?.length&&formStages.has(stageKey('generic',field.key))).map(field=>({key:field.key,value:genericValue(field)}));}
function currentNativeStages() {return (nativeFormForView()?.page===view?nativeFormForView().fields:[]).filter(field=>field.enabled&&['text','secret'].includes(field.kind)&&formStages.has(stageKey('native',field.id))).map(field=>({id:field.id,value:nativeValue(field)}));}
function stageText(input) {
 if(!input||closing)return false;
 let key,record,baseline;
 if(input.dataset?.nativeField!==undefined&&['text','secret'].includes(input.dataset.nativeKind)){
  const field=nativeFormForView()?.fields.find(field=>String(field.id)===input.dataset.nativeField);if(!field)return false;
  key=stageKey('native',field.id);baseline=field.kind==='secret'?'':String(field.value??'');record={kind:'native',page:view,id:field.id,value:input.value,secret:field.kind==='secret'};
 }else if(input.dataset?.field&&input.tagName==='INPUT'){
  const field=state?.editor?.fields.find(field=>field.key===input.dataset.field);if(!field)return false;
  key=stageKey('generic',field.key);baseline=String(field.value??'');record={kind:'generic',page:state.editor.page,index:state.editor.index,key:field.key,value:input.value};
 }else return false;
 if(record.value===baseline)formStages.delete(key);else formStages.set(key,record);
 syncControls();return true;
}
function stagedDirty() {return formStages.size>0;}
function currentPageStaged() {return [...formStages.values()].some(record=>record.page===view);}
function discardPageFields() {const removed=new Set();for(const [key,record] of formStages)if(record.page===view){formStages.delete(key);removed.add(key);}submittedStages=submittedStages.filter(([key])=>!removed.has(key));render();}
function completeStageAck() {for(const [key,value] of submittedStages)if(formStages.get(key)?.value===value)formStages.delete(key);submittedStages=[];}
function selectTarget(index,input=null) {if(stagedDirty()){if(input)input.value=String(state.editor.index);showError('Примените или отмените незавершённый ввод перед выбором другого объекта.');return;}send({op:'select_item',index},true);}
function deferRender() {if(pointerGesture||keyGesture||composing){renderQueued=true;return true;}return false;}
function releaseGesture() {requestAnimationFrame(()=>{if(renderQueued&&!pointerGesture&&!keyGesture&&!composing){renderQueued=false;render();}});}
function nativeControlDisabled(node) {
 const items=node.dataset.nativeAction!==undefined?nativeFormForView()?.actions:nativeFormForView()?.fields;
 const id=node.dataset.nativeAction??node.dataset.nativeField;
 return closing || commandPending || !items?.some(item=>String(item.id)===id&&item.enabled);
}
function showError(message,fromCommand=false) {errorIsCommand=fromCommand;error=String(message);const box=$('#native-error');box.hidden=false;box.innerHTML=`<button class="icon-button" data-action="dismiss-error" aria-label="Закрыть сообщение">×</button><p>${esc(error)}</p>${stagedDirty()?'<button class="button" data-action="discard-fields">Отменить незавершённый ввод</button>':''}<button class="button" data-action="retry-document" data-mutation>Повторить</button>`;}
function send(command, mutation=false) {
 if(closing)return false;
 const nativeCommand=['native_field','native_action'].includes(command.op);
 if(nativeCommand){const node={dataset:{[command.op==='native_field'?'nativeField':'nativeAction']:String(command.id)}};if(nativeControlDisabled(node))return false;const items=command.op==='native_field'?nativeFormForView().fields:nativeFormForView().actions;command.id=items.find(item=>String(item.id)===String(command.id)).id;}
 else if(mutation && busy() && !recoveryOperation(command.op)) return false;
 if(['open','save','export'].includes(command.op)&&stagedDirty()){showError('Примените или отмените незавершённый ввод перед сохранением профиля.');return false;}
 const stagedCommand=['apply_fields','set_field','asset','native_field','native_action'].includes(command.op);
 const discardNative=nativeCommand&&['408','1812','1819','1107','1141'].includes(String(command.id));
 if(stagedCommand&&!discardNative){if(nativeCommand){command.fields=currentNativeStages();command.editor_fields=state.pending?[]:currentGenericStages().filter(item=>state.editor.fields.find(field=>field.key===item.key)?.enabled!==false);}else command.fields=currentGenericStages();}
 if(['apply_scene','save_scene','open','save','export'].includes(command.op))flushPreview(true);
 if(!window.ipc || typeof window.ipc.postMessage !== 'function') {showError('Нет подключения к приложению Prismestra. Откройте интерфейс из нативного приложения.');return false;}
 try {window.ipc.postMessage(JSON.stringify(command));if(mutation||command.op==='page')awaitingCommandAck=true;if(stagedCommand){const keys=discardNative?[...formStages].filter(([,record])=>record.page===view).map(([key])=>key):nativeCommand?[...(command.fields||[]).map(item=>stageKey('native',item.id)),...(command.editor_fields||[]).map(item=>stageKey('generic',item.key))]:(command.fields||[]).map(item=>stageKey('generic',item.key));submittedStages=keys.filter(key=>formStages.has(key)).map(key=>[key,formStages.get(key).value]);if(nativeCommand&&!discardNative){for(const [key] of submittedStages)if(formStages.get(key)?.secret)formStages.delete(key);document.querySelectorAll('[data-native-kind="secret"]').forEach(input=>{if(!formStages.has(stageKey('native',input.dataset.nativeField)))input.value='';});}}if(mutation){commandPending=true;nativeAwaitingAck=nativeCommand||stagedCommand;syncControls();}return true;}catch(e){awaitingCommandAck=false;commandPending=false;showError(e.message);syncControls();return false;}
}
function syncControls() {
 document.querySelectorAll('[data-mutation]').forEach(node=>node.disabled=controlDisabled(node));
 document.querySelectorAll('[data-native-control]').forEach(node=>node.disabled=nativeControlDisabled(node));
 document.querySelectorAll('[data-local-discard]').forEach(node=>{node.hidden=!currentPageStaged();node.disabled=closing||commandPending;});
 $('#master-brightness').disabled=busy()||!draft;
 $('#footer-apply').disabled=busy()||!changed();
 $('#play').disabled=busy();
 const light=!!(state?.groups?.pc||state?.groups?.door);$('#play').textContent=light?'Выключить свет':'Включить свет';
 $('#player-name').textContent=state?(effects[state.scene.effect]||state.scene.effect):'Prismestra';
 $('#player-status').textContent=state?.status||'Подключение к приложению…';
 $('#save-status').textContent=busy()&&state?'Выполняется…':state?.dirty?'Профиль изменён':'Локальный профиль';
 if(draft){$('#master-brightness').value=draft.brightness;$('#master-value').textContent=draft.brightness+'%';}
 const status=$('.scene-status');if(status)status.textContent=changed()?'Изменения ещё не применены':'Эта версия применена';
 const apply=$('[data-action="apply-scene"]');if(apply)apply.textContent=changed()?'Применить':'Применено';
 if(state){const art=$('#player-art'),ctx=art.getContext('2d');art.setAttribute('aria-label','Палитра применённой сцены');state.scene.colors.forEach((c,i)=>{ctx.fillStyle=hex(c);ctx.fillRect(i*art.width/state.scene.colors.length,0,Math.ceil(art.width/state.scene.colors.length),art.height);});}
 if(state?.error)showError(state.error,true);
}
function palette(scene) {return `<div class="palette-art" aria-label="Палитра сцены">${scene.colors.map(c=>`<span style="background:${hex(c)}"></span>`).join('')}</div>`;}
function heading(title,description,actions='') {return `<div class="page-heading"><div><h1>${esc(title)}</h1><p>${esc(description)}</p></div><div class="heading-actions">${actions}</div></div>`;}
function action(label,code,primary=false,extra='') {return `<button class="button ${primary?'primary':''}" data-action="${code}" data-mutation ${disabled()} ${extra}>${label}</button>`;}
function renderLibrary() {
 const list=state.library.filter(item=>item.name.toLocaleLowerCase('ru').includes(search.toLocaleLowerCase('ru')));
 return heading('Мои сцены','Сцены сохраняют параметры RGB. Устройства и LCD сохраняются в профиле.',action(icon('plus')+' Новая сцена','new',true))+`<div class="toolbar"><div class="filters"><span class="filter active">Все сцены · ${state.library.length}</span></div><label class="search"><input id="search" placeholder="Найти сцену" aria-label="Найти сцену" value="${esc(search)}"></label></div><div class="library-grid">${list.map(item=>`<article class="scene-card ${same(item.scene,state.scene)?'selected':''}"><div class="art-wrap"><button class="scene-preview" data-edit="${esc(item.id)}" aria-label="Открыть: ${esc(item.name)}" data-mutation ${disabled()}>${item.preview?`<canvas data-library-preview="${esc(item.id)}" aria-label="Предпросмотр: ${esc(item.name)}"></canvas>`:palette(item.scene)}</button></div><div class="scene-body"><div class="scene-title"><h2>${esc(item.name)}</h2></div><div class="card-actions"><button class="button" data-apply="${esc(item.id)}" aria-label="Применить: ${esc(item.name)}" title="Применить" data-mutation ${disabled()}>${icon('play')}</button><button class="button" data-edit="${esc(item.id)}" aria-label="Редактировать: ${esc(item.name)}" data-mutation ${disabled()}>${icon('edit')}</button><button class="icon-button" data-remove="${esc(item.id)}" aria-label="Удалить: ${esc(item.name)}" data-mutation ${disabled()}>${icon('trash')}</button></div></div></article>`).join('')||'<div class="empty"><h2>Сцен не найдено</h2><p>Сохраните текущую сцену или измените запрос.</p></div>'}</div>`;
}
function range(key,label,min,max,step=1,suffix='') {
 const value=key.startsWith('noise.')?draft.noise[key.slice(6)]:draft[key];
 return `<div class="control-block"><label class="control-label" for="param-${key}"><span>${label}</span><output id="value-${key}">${value}${suffix}</output></label><input id="param-${key}" type="range" data-param="${key}" data-suffix="${suffix}" min="${min}" max="${max}" step="${step}" value="${value}" data-mutation ${disabled()}></div>`;
}
function renderScene() {
 const noise=draft.effect==='noise_map';
 return heading(draftName,'Изменения остаются в черновике до нажатия «Применить».',action('Сохранить сцену','save-scene')+action('Создать копию','clone')+(selectedId?action('Переименовать','rename'):'')+action('Применить','apply-scene',true,'data-needs-change="scene"'))+`<p class="scene-status"></p><div class="editor-layout"><section class="preview-panel" aria-label="Предпросмотр"><div class="stage"><canvas id="stage" aria-label="Предпросмотр сцены из движка Prismestra"></canvas><div class="stage-bar"><span class="stage-chip">Предпросмотр</span></div><div class="stage-caption">Кадр движка · черновик</div></div><p class="preview-description">Предпросмотр не меняет подсветку. Выходы RGB и LCD включаются отдельно.</p><div class="effect-strip-label">Эффект</div><div class="effect-strip">${Object.entries(effects).map(([key,name])=>`<button class="strip-item native-effect-choice ${draft.effect===key?'active':''}" data-effect="${key}" aria-pressed="${draft.effect===key}" data-mutation ${disabled()}><span>${name}</span></button>`).join('')}</div><div class="parameter-panel" style="margin-top:20px"><h2>Выходы</h2><label class="native-group"><input type="checkbox" data-group="pc" ${state.groups.pc?'checked':''} data-mutation ${disabled()}> Основной свет · RGB и LCD</label><label class="native-group"><input type="checkbox" data-group="door" ${state.groups.door?'checked':''} data-mutation ${disabled()}> Дополнительный свет</label><button class="button" data-view="devices">Устройства и свойства</button></div></section><aside class="parameter-panel" aria-label="Параметры сцены"><div class="control-block"><div class="control-label">Палитра <small>${draft.colors.length} / 8</small></div><div class="native-palette">${draft.colors.map((c,i)=>`<div class="native-palette-item"><label class="color-control"><input type="color" data-color="${i}" value="${hex(c)}" aria-label="Цвет ${i+1}" data-mutation ${disabled()}></label><button class="icon-button" data-remove-color="${i}" data-mutation aria-label="Удалить цвет ${i+1}" ${draft.colors.length===1?'disabled':disabled()}>×</button></div>`).join('')}<button class="icon-button" data-action="add-color" aria-label="Добавить цвет" data-mutation ${draft.colors.length>=8?'disabled':disabled()}>${icon('plus')}</button></div></div>${range('speed','Скорость',draft.effect==='breathing'?10:1,draft.effect==='breathing'?200:100)}${range('brightness','Яркость',0,100,1,'%')}<details id="advanced-scene" ${advancedOpen?'open':''}><summary>Дополнительно</summary>${range('fps','Частота кадров',1,144,1,' fps')}${range('temperature','Температура',-100,100)}${range('tint','Оттенок',-100,100)}${draft.effect==='rainbow_wave'?range('rainbow_width','Ширина волны',1,50):''}${noise?`<div class="native-field control-block"><label for="noise-palette">Палитра шума</label><select id="noise-palette" data-scene-select="noise_palette" data-mutation ${disabled()}>${Object.entries({custom:'Своя палитра',rainbow:'Радуга',inverse_rainbow:'Обратная радуга'}).map(([v,t])=>`<option value="${v}" ${draft.noise_palette===v?'selected':''}>${t}</option>`).join('')}</select></div>${range('noise.frequency','Частота шума',0.0001,0.5,0.0001)}${range('noise.amplitude','Амплитуда',0.0001,5,0.0001)}${range('noise.lacunarity','Лакунарность',0.0001,5,0.0001)}${range('noise.persistence','Затухание',0.0001,5,0.0001)}${range('noise.octaves','Октавы',1,20)}${range('noise.motion','Направление движения',0,3)}${range('noise.motion_speed','Скорость движения',0,99)}`:''}</details></aside></div>`;
}
function nativeProperties() {
 const editor=state.editor;
 if(!editor || editor.page!==view)return state.nativeAux?.page===view?renderNativeForm(state.nativeAux):'<div class="native-wait">Настройки загружаются…</div>';
 return `<section class="native-properties"><div class="native-inline"><label for="native-target">Объект</label><select id="native-target" class="native-target" data-target data-mutation ${disabled()}>${editor.targets.map((name,i)=>`<option value="${i}" ${i===editor.index?'selected':''}>${esc(name)}</option>`).join('')}</select></div><div class="parameter-panel"><div class="native-form">${editor.fields.map((field,i)=>`<div class="native-field"><label for="field-${i}">${esc(field.label)}</label>${field.choices?.length?`<select id="field-${i}" data-field="${esc(field.key)}" data-mutation ${disabled()}>${field.choices.map(choice=>`<option value="${esc(choice.value)}" ${String(choice.value)===String(field.value)?'selected':''}>${esc(choice.label)}</option>`).join('')}</select>`:`<div class="native-field-file"><input id="field-${i}" data-field="${esc(field.key)}" value="${esc(genericValue(field))}" data-mutation ${disabled()}>${field.file?`<button class="button" data-asset="${esc(field.key)}" data-mutation ${disabled()}>Выбрать…</button>`:''}</div>`}${field.hint?`<small>${esc(field.hint)}</small>`:''}</div>`).join('')||'<p class="muted">Параметров объекта нет.</p>'}</div><div class="native-property-actions">${action('Применить','apply-document',true,'data-needs-change="document"')}${action('Отменить изменения','revert-document')}${action('Повторить','retry-document')}</div></div></section>${state.nativeAux?.page===view?renderNativeForm(state.nativeAux):''}`;
}
function renderDevices() {
 return heading('Устройства','Изменения действуют после «Применить». Профиль сохраняется отдельно.')+`<div class="device-list">${state.devices.map((d,index)=>`<article class="device"><div class="device-symbol">${icon('devices')}</div><div class="device-info"><h2>${esc(d.label)}</h2><p>${esc(d.driver?.type)} · ${esc(d.role)}</p></div><div class="device-meta"><span>${esc(d.leds)} LED · ${esc(d.rate_hz)} Гц</span><small>${d.enabled?'Включено':'Выключено'}</small></div><button class="icon-button" data-device-index="${index}" data-mutation ${disabled()} aria-label="Свойства: ${esc(d.label)}">${icon('edit')}</button></article>`).join('')||'<div class="empty"><h2>В профиле нет устройств</h2><p>Добавьте устройство в свойствах профиля.</p></div>'}</div><div style="margin-top:24px">${nativeProperties()}</div>`;
}
function renderNativeForm(form=nativeFormForView()) {
 if(!form||form.page!==view)return '<div class="native-wait" role="status">Загрузка настроек…</div>';
 return `<section class="native-properties"><div class="parameter-panel"><div class="native-form">${form.fields.map((field,index)=>{
  const id='native-field-'+index,common=`id="${id}" data-native-field="${esc(field.id)}" data-native-kind="${esc(field.kind)}" data-native-control ${nativeControlDisabled({dataset:{nativeField:String(field.id)}})?'disabled':''}`;
  if(field.kind==='checkbox')return `<label class="native-group" for="${id}"><input type="checkbox" ${common} ${field.value==='true'?'checked':''}>${esc(field.label)}</label>`;
  const control=field.kind==='select'?`<select ${common}>${!field.choices.some(choice=>String(choice.value)===String(field.value))?'<option value="" disabled selected>Выберите…</option>':''}${field.choices.map(choice=>`<option value="${esc(choice.value)}" ${String(choice.value)===field.value?'selected':''}>${esc(choice.label)}</option>`).join('')}</select>`:`<input ${common} type="${field.kind==='secret'?'password':'text'}" autocomplete="off" value="${field.kind==='secret'?'':esc(nativeValue(field))}">`;
  return `<div class="native-field"><label for="${id}">${esc(field.label)}</label>${control}</div>`;
 }).join('')}</div><div class="native-property-actions">${form.actions.map(item=>`<button class="button" data-native-action="${esc(item.id)}" data-native-control ${nativeControlDisabled({dataset:{nativeAction:String(item.id)}})?'disabled':''}>${esc(item.label)}</button>`).join('')}<button class="button" data-action="discard-page-fields" data-local-discard ${currentPageStaged()?'':'hidden'}>Отменить незавершённый ввод</button></div>${form.notes.map(note=>`<p class="parameter-note">${esc(note)}</p>`).join('')}</div></section>`;
}
function render() {
 if(deferRender())return;renderQueued=false;
 const key=focusKey(),node=document.activeElement,selection=typeof node?.selectionStart==='number'?[node.selectionStart,node.selectionEnd]:null;
 $('nav').innerHTML=Object.entries(pages).map(([key,title])=>`<button data-view="${key}" class="${view===key?'active':''}" aria-label="${title}" title="${title}" ${view===key?'aria-current="page"':''}>${icon(key)}<span>${title}</span></button>`).join('');
 if(!state)$('#main').innerHTML='<div class="native-wait"><h1>Подключение к Prismestra…</h1><p>Ожидание локального профиля.</p></div>';
 else {$('#main').innerHTML=view==='library'?renderLibrary():view==='editor'?renderScene():view==='devices'?renderDevices():heading(pages[view],'Настройки локального профиля.')+(advancedPage()?renderNativeForm():nativeProperties());$('.project-context').innerHTML=`<span>Локальный профиль</span><small title="${esc(state.profilePath||'')}">${esc(state.profilePath?.split(/[\\/]/).pop()||'Не сохранён')}</small>`;}
 document.querySelectorAll('[data-native-kind="secret"]').forEach(input=>{const staged=formStages.get(stageKey('native',input.dataset.nativeField));if(staged)input.value=staged.value;});
 syncControls();paintPreview();paintLibraryPreviews();restoreFocus(key,selection);
}
function flushPreview(force=false) {
 clearTimeout(previewTimer);previewTimer=null;
 if(!draft||(!previewQueued&&!force))return;
 previewQueued=true;
 if(send({op:'preview_scene',scene:clone(draft)}))previewQueued=false;
}
function requestPreview() {if(closing)return;previewQueued=true;clearTimeout(previewTimer);previewTimer=setTimeout(()=>flushPreview(),90);}
function editDraft() {syncControls();requestPreview();}
function paintPixels(canvas,pixelsFrame) {
 if(!canvas || !pixelsFrame || !Number.isInteger(pixelsFrame.width)||!Number.isInteger(pixelsFrame.height)||pixelsFrame.width<1||pixelsFrame.height<1||pixelsFrame.width*pixelsFrame.height>1048576||!Array.isArray(pixelsFrame.colors)||pixelsFrame.colors.length!==pixelsFrame.width*pixelsFrame.height)return;
 canvas.width=pixelsFrame.width;canvas.height=pixelsFrame.height;
 const ctx=canvas.getContext('2d'),pixels=ctx.createImageData(pixelsFrame.width,pixelsFrame.height);
 pixelsFrame.colors.forEach((c,i)=>{pixels.data[i*4]=(c>>>16)&255;pixels.data[i*4+1]=(c>>>8)&255;pixels.data[i*4+2]=c&255;pixels.data[i*4+3]=255;});ctx.putImageData(pixels,0,0);
}
function paintPreview() {paintPixels($('#stage'),frame);}
function paintLibraryPreviews() {
 document.querySelectorAll('[data-library-preview]').forEach(canvas=>paintPixels(canvas,state?.library.find(item=>String(item.id)===canvas.dataset.libraryPreview)?.preview));
}
function changeView(next) {
 if(!Object.hasOwn(pages,next)||next===view||closing||pendingView)return;
 if(stagedDirty()||(state?.nativeForm?.page===view&&state.nativeForm.dirty)||(state?.editor?.dirty&&state.editor.page===view)){showError('Примените или отмените изменения перед переходом в другой раздел.');return;}
 const previous=view;view=next;render();focusHeading();
 if(!['library','editor'].includes(next)){
  pendingView={previous,next};
  if(!send({op:'page',page:next})){view=previous;pendingView=null;render();focusHeading();}
 }
 if(next==='editor')requestPreview();
}
function openEntry(entry) {draft=clone(entry.scene);draftName=entry.name;selectedId=entry.id;changeView('editor');}
function nameDialog(mode) {
 if(!draft||busy())return;
 dialogMode=mode;dialogReturn=focusKey();$('#dialog-title').textContent={new:'Новая сцена',clone:'Создать копию',rename:'Переименовать сцену',save:'Сохранить сцену'}[mode];
 $('#scene-name').value=mode==='new'?'':mode==='clone'?(draftName+' · копия').slice(0,48):draftName;$('#scene-name').setCustomValidity('');$('#name-dialog').showModal();$('#scene-name').focus();$('#scene-name').select();
}
window.prismestraReceive = message => {
 try {
  if(typeof message==='string')message=JSON.parse(message);
  if(message.kind==='flush'){
   if(!Number.isSafeInteger(message.id)||message.id<0)throw Error('Некорректный запрос закрытия');
   stageText(document.activeElement);closing=true;clearTimeout(previewTimer);previewTimer=null;previewQueued=false;syncControls();
   window.ipc.postMessage(JSON.stringify({op:'close_ready',id:message.id,scene:draft?clone(draft):null,form_dirty:stagedDirty()}));return;
  }
  if(message.kind==='close_cancelled'){closing=false;syncControls();return;}
  if(message.kind==='error'){awaitingCommandAck=false;closing=false;submittedStages=[];savedSceneRequest=null;commandPending=false;nativeAwaitingAck=false;if(pendingView){view=pendingView.previous;pendingView=null;render();focusHeading();}showError(message.message,true);syncControls();return;}
  if(message.kind==='snapshot') {
   const acknowledged=awaitingCommandAck;awaitingCommandAck=false;completeStageAck();const previous=state;state=message.state;
   if(acknowledged&&errorIsCommand&&!state.error){error='';errorIsCommand=false;$('#native-error').hidden=true;$('#native-error').innerHTML='';}commandPending=false;nativeAwaitingAck=false;pendingView=null;
   if(!draft){draft=clone(state.draftScene||state.scene);}else if(previous && same(draft,previous.scene)){draft=clone(state.scene);if(!selectedId)draftName='Текущая сцена';}
   if(savedSceneRequest){const saved=state.library.find(item=>(savedSceneRequest.id?item.id===savedSceneRequest.id:!savedSceneRequest.ids.includes(item.id))&&item.name===savedSceneRequest.name&&same(item.scene,savedSceneRequest.scene));if(saved){selectedId=saved.id;draftName=saved.name;savedSceneRequest=null;}}
   if(selectedId && !state.library.some(item=>item.id===selectedId))selectedId=null;
   render();if(!busy())flushPreview();
  } else if(message.kind==='status'&&state) {
   const oldScene=state.scene,nativeFormChanged=(Object.hasOwn(message.state,'nativeForm')&&!same(state.nativeForm,message.state.nativeForm))||(Object.hasOwn(message.state,'nativeAux')&&!same(state.nativeAux,message.state.nativeAux));Object.assign(state,message.state);
   if(!changed()||same(draft,oldScene))draft=clone(state.scene);
   if(!state.pending&&!nativeAwaitingAck)commandPending=false;
   if(message.state.preview)frame=message.state.preview;
   if(nativeFormChanged)render();else {syncControls();paintPreview();}if(!busy())flushPreview();
  }
 }catch(e){awaitingCommandAck=false;closing=false;commandPending=false;showError('Ошибка ответа приложения: '+e.message);syncControls();}
};
document.addEventListener('click',event=>{
 const b=event.target.closest('button');if(!b||b.disabled||(busy()&&b.hasAttribute('data-mutation')&&!recoveryAction(b.dataset.action)))return;
 if(b.dataset.nativeAction!==undefined)return void send({op:'native_action',id:b.dataset.nativeAction},true);
 if(b.dataset.view)return changeView(b.dataset.view);
 if(b.dataset.edit){const entry=state.library.find(e=>e.id===b.dataset.edit);if(entry)openEntry(entry);return;}
 if(b.dataset.apply){const entry=state.library.find(e=>e.id===b.dataset.apply);if(entry)send({op:'apply_scene',scene:clone(entry.scene)},true);return;}
 if(b.dataset.remove){const entry=state.library.find(e=>e.id===b.dataset.remove);if(entry){dialogReturn=focusKey();$('#remove-description').textContent=`Удалить сцену «${entry.name}» из профиля? Запущенный эффект продолжит работать.`;$('#remove-dialog').dataset.id=entry.id;$('#remove-dialog').showModal();$('#cancel-remove').focus();}return;}
 if(b.dataset.effect){draft.effect=b.dataset.effect;draft.speed=Math.min(draft.effect==='breathing'?200:100,Math.max(draft.effect==='breathing'?10:1,draft.speed));render();requestPreview();return;}
 if(b.dataset.removeColor!==undefined){if(draft.colors.length>1)draft.colors.splice(+b.dataset.removeColor,1);render();requestPreview();return;}
 if(b.dataset.asset)return void send({op:'asset',key:b.dataset.asset},true);
 if(b.dataset.deviceIndex!==undefined)return void selectTarget(+b.dataset.deviceIndex);
 switch(b.dataset.action){
 case 'new':case 'clone':case 'rename':return nameDialog(b.dataset.action);
 case 'save-scene':return nameDialog('save');
 case 'apply-scene':send({op:'apply_scene',scene:clone(draft)},true);break;
 case 'add-color':if(draft.colors.length<8)draft.colors.push(draft.colors.at(-1));render();requestPreview();break;
 case 'apply-document':send({op:'apply_fields'},true);break;
 case 'revert-document':formStages.clear();submittedStages=[];if(state.editor?.dirty||state.pending||commandPending)send({op:'revert_document'},true);else render();break;
 case 'discard-page-fields':discardPageFields();break;
 case 'discard-fields':discardPageFields();showError('Незавершённый ввод отменён.');break;
 case 'retry-document':send({op:'retry_document'},true);break;
 case 'classic':send({op:'classic',page:view},true);break;
 case 'dismiss-error':error='';errorIsCommand=false;$('#native-error').hidden=true;if(state)state.error=null;break;
 case 'open':case 'save':case 'export':send({op:b.dataset.action},true);break;
 }
});
document.addEventListener('input',event=>{
 const input=event.target;if(stageText(input))return;if(busy()||!draft)return;
 if(input.id==='search'){search=input.value;render();return;}
 if(input.dataset.param){const n=Number(input.value);if(!Number.isFinite(n))return;const key=input.dataset.param;if(key.startsWith('noise.'))draft.noise[key.slice(6)]=n;else draft[key]=n;document.getElementById('value-'+key).textContent=n+input.dataset.suffix;editDraft();}
 if(input.dataset.color!==undefined){draft.colors[+input.dataset.color]=parseInt(input.value.slice(1),16);editDraft();}
 if(input.id==='master-brightness'){draft.brightness=+input.value;const slider=$('[data-param="brightness"]');if(slider){slider.value=draft.brightness;document.getElementById('value-brightness').textContent=draft.brightness+'%';}editDraft();}
});
document.addEventListener('change',event=>{
 const input=event.target;
 if(input.dataset.nativeField!==undefined){if(['text','secret'].includes(input.dataset.nativeKind)){stageText(input);return;}if(nativeControlDisabled(input))return;send({op:'native_field',id:input.dataset.nativeField,value:input.dataset.nativeKind==='checkbox'?String(input.checked):input.value},true);return;}
 if(busy())return;
 if(input.dataset.group){send({op:'groups',...state.groups,[input.dataset.group]:input.checked},true);return;}
 if(input.hasAttribute('data-target'))return void selectTarget(+input.value,input);
 if(input.dataset.field){if(input.tagName==='INPUT'){stageText(input);return;}return void send({op:'set_field',key:input.dataset.field,value:input.value},true);}
 if(input.dataset.sceneSelect){draft[input.dataset.sceneSelect]=input.value;editDraft();}
});
$('#play').onclick=()=>{if(!state)return;const on=!(state.groups.pc||state.groups.door);send({op:'groups',pc:on,door:on},true);};
$('#footer-apply').onclick=()=>{if(draft)send({op:'apply_scene',scene:clone(draft)},true);};
$('#open-active').onclick=()=>changeView('editor');
$('.brand').onclick=event=>{event.preventDefault();changeView('library');};
$('#sun-icon').innerHTML=icon('sun');
$('#name-form').onsubmit=event=>{
 event.preventDefault();if(busy())return;
 const name=$('#scene-name').value.trim();if(!name||name.length>48||/[\u0000-\u001f\u007f]/.test(name)){$('#scene-name').setCustomValidity('Введите название до 48 символов без управляющих знаков');$('#scene-name').reportValidity();return;}
 const command={op:'save_scene',name,scene:clone(draft)};if(['save','rename'].includes(dialogMode)&&selectedId)command.id=selectedId;
 savedSceneRequest={id:command.id,name,scene:clone(draft),ids:state.library.map(item=>item.id)};
 if(send(command,true)){draftName=name;if(['new','clone'].includes(dialogMode))selectedId=null;$('#name-dialog').close();restoreFocus(dialogReturn);}
};
$('#scene-name').oninput=()=>$('#scene-name').setCustomValidity('');
$('#cancel-name').onclick=()=>{$('#name-dialog').close();restoreFocus(dialogReturn);};
$('#name-dialog').addEventListener('cancel',()=>restoreFocus(dialogReturn));
const removal=document.createElement('dialog');removal.id='remove-dialog';removal.setAttribute('aria-labelledby','remove-title');removal.innerHTML='<form id="remove-form"><h2 id="remove-title">Удалить сцену</h2><p id="remove-description"></p><div class="dialog-actions"><button type="button" class="button" id="cancel-remove">Отмена</button><button type="submit" class="button primary" data-mutation>Удалить</button></div></form>';document.body.append(removal);
$('#cancel-remove').onclick=()=>{removal.close();restoreFocus(dialogReturn);};removal.addEventListener('cancel',()=>restoreFocus(dialogReturn));$('#remove-form').onsubmit=event=>{event.preventDefault();if(send({op:'delete_scene',id:removal.dataset.id},true)){removal.close();restoreFocus(dialogReturn);}};
const projectActions=document.createElement('div');projectActions.className='native-project-actions';projectActions.innerHTML=action('Открыть','open')+action('Сохранить','save')+action('Экспорт','export');$('#save-status').before(projectActions);
document.addEventListener('pointerdown',()=>{pointerGesture=true;},true);
document.addEventListener('pointerup',()=>{pointerGesture=false;releaseGesture();},true);
document.addEventListener('pointercancel',()=>{pointerGesture=false;releaseGesture();},true);
document.addEventListener('keydown',event=>{if((event.key===' '||event.key==='Spacebar')&&event.target.closest('button'))keyGesture=true;},true);
document.addEventListener('keyup',()=>{keyGesture=false;releaseGesture();},true);
document.addEventListener('compositionstart',()=>{composing=true;},true);
document.addEventListener('compositionend',()=>{composing=false;releaseGesture();},true);
window.addEventListener('blur',()=>{pointerGesture=false;keyGesture=false;composing=false;releaseGesture();});
document.addEventListener('toggle',event=>{if(event.target.id==='advanced-scene')advancedOpen=event.target.open;},true);
window.addEventListener('pagehide',()=>flushPreview(true));
window.addEventListener('beforeunload',()=>flushPreview(true));
render();send({op:'snapshot'});
