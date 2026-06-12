const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('toast', {
  onAdd: (cb) => ipcRenderer.on('toast-add', (_e, data) => cb(data)),
  onUpdate: (cb) => ipcRenderer.on('toast-update', (_e, data) => cb(data)),
  click: (sessionId) => ipcRenderer.invoke('toast:click', sessionId),
  resize: (h) => ipcRenderer.invoke('toast:resize', h),
});
