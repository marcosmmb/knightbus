import http from 'node:http';
import {readFile} from 'node:fs/promises';
const PORT = 4178;
const TYPES = {html:'text/html',css:'text/css',js:'application/javascript',svg:'image/svg+xml'};
http.createServer(async (req,res) => {
  const file = req.url === '/' ? 'index.html' : req.url.slice(1);
  if (!['index.html','style.css','app.js','connection.js','tooltips.js','icon.svg'].includes(file)) {res.writeHead(404).end(); return;}
  try {res.setHeader('Content-Type',TYPES[file.split('.').pop()]); res.end(await readFile(new URL(`../frontend/${file}`,import.meta.url)));}
  catch {res.writeHead(500).end();}
}).listen(PORT,'127.0.0.1');
