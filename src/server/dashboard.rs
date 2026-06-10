use crate::error::{LabyrinthError, Result};
use crate::protocol::AgentKind;
use crate::server::core::{
    ConnectedAgent, FullhouseSnapshot, LabyrinthServer, PortForwardSnapshot,
};
use crate::server::dweller_registry::{DwellerRecord, DwellerRegistry};
use crate::server::topology::{TopologyManager, TopologySnapshot};
use colored::Colorize;
use serde::Serialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{timeout, Duration};
use tracing::{error, info};

const DASHBOARD_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Labyrinth Network Map</title>
  <style>
    :root {
      color-scheme: dark;
      --bg: #0e1116;
      --panel: #151a21;
      --panel-2: #1b222c;
      --panel-3: #202936;
      --text: #eef4f8;
      --muted: #99a6b3;
      --faint: #667586;
      --line: #2c3643;
      --line-strong: #3b4858;
      --ok: #1fe0a1;
      --warn: #ffd05a;
      --bad: #ff6673;
      --accent: #46a6ff;
      --agent: #23d18b;
      --dweller: #d95cff;
      --network: #f6c945;
      --forward: #ff8b5c;
      --shadow: rgba(0, 0, 0, .42);
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      min-height: 100vh;
      background: var(--bg);
      color: var(--text);
      font: 14px/1.45 Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    }
    .shell {
      min-height: 100vh;
      display: grid;
      grid-template-rows: auto 1fr;
    }
    header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 18px;
      padding: 14px 20px;
      border-bottom: 1px solid var(--line);
      background: rgba(17, 22, 29, .96);
      box-shadow: 0 12px 28px rgba(0, 0, 0, .2);
      position: relative;
      z-index: 3;
    }
    .brand {
      display: flex;
      align-items: center;
      gap: 12px;
      min-width: 0;
    }
    .brand-mark {
      width: 34px;
      height: 34px;
      border-radius: 8px;
      display: grid;
      place-items: center;
      background: linear-gradient(145deg, rgba(70, 166, 255, .22), rgba(31, 224, 161, .14));
      border: 1px solid var(--line-strong);
      box-shadow: inset 0 0 18px rgba(70, 166, 255, .08);
      flex: 0 0 auto;
    }
    .brand h1 {
      margin: 0;
      font-size: 18px;
      font-weight: 720;
      letter-spacing: 0;
    }
    .brand span {
      display: block;
      color: var(--muted);
      font-size: 12px;
      margin-top: 1px;
    }
    .header-meta {
      display: flex;
      align-items: center;
      gap: 12px;
      color: var(--muted);
      white-space: nowrap;
    }
    .status-chip {
      display: inline-flex;
      align-items: center;
      gap: 8px;
      min-height: 30px;
      padding: 4px 10px;
      border: 1px solid var(--line);
      border-radius: 999px;
      background: #10151b;
    }
    .status-dot {
      width: 9px;
      height: 9px;
      border-radius: 999px;
      background: var(--bad);
      display: inline-block;
      box-shadow: 0 0 0 4px rgba(255, 102, 115, .12);
    }
    .status-dot.online {
      background: var(--ok);
      box-shadow: 0 0 0 4px rgba(31, 224, 161, .12), 0 0 18px rgba(31, 224, 161, .34);
    }
    main {
      display: grid;
      grid-template-columns: minmax(0, 1fr) 380px;
      min-height: 0;
    }
    #map-wrap {
      position: relative;
      min-height: calc(100vh - 64px);
      overflow: hidden;
      background:
        radial-gradient(circle at 34% 44%, rgba(70, 166, 255, .09), transparent 28%),
        linear-gradient(rgba(255, 255, 255, .025) 1px, transparent 1px),
        linear-gradient(90deg, rgba(255, 255, 255, .025) 1px, transparent 1px),
        var(--bg);
      background-size: auto, 34px 34px, 34px 34px, auto;
    }
    #map {
      width: 100%;
      height: 100%;
      min-height: calc(100vh - 64px);
      display: block;
    }
    .map-toolbar {
      position: absolute;
      left: 18px;
      top: 18px;
      display: flex;
      gap: 8px;
      z-index: 2;
    }
    .tool {
      min-height: 32px;
      display: inline-flex;
      align-items: center;
      gap: 7px;
      border: 1px solid var(--line);
      border-radius: 8px;
      padding: 5px 9px;
      color: var(--muted);
      background: rgba(18, 23, 30, .9);
      backdrop-filter: blur(10px);
      box-shadow: 0 8px 20px rgba(0, 0, 0, .2);
      font-size: 12px;
    }
    .tool svg { color: var(--accent); }
    .empty {
      position: absolute;
      inset: 0;
      display: none;
      align-items: flex-start;
      justify-content: center;
      color: var(--muted);
      text-align: center;
      padding: 132px 24px 24px;
      pointer-events: none;
    }
    .empty-panel {
      width: min(430px, calc(100vw - 48px));
      border: 1px solid var(--line);
      border-radius: 8px;
      background: rgba(20, 26, 34, .82);
      box-shadow: 0 18px 44px var(--shadow);
      padding: 18px;
    }
    .empty-title {
      color: var(--text);
      font-weight: 700;
      font-size: 15px;
      margin-bottom: 4px;
    }
    aside {
      min-height: 0;
      overflow: auto;
      border-left: 1px solid var(--line);
      background: linear-gradient(180deg, var(--panel), #12171d);
      padding: 18px;
    }
    .section { margin-bottom: 20px; }
    .section h2 {
      margin: 0 0 10px;
      font-size: 12px;
      text-transform: uppercase;
      color: var(--muted);
      font-weight: 760;
      letter-spacing: .06em;
    }
    .metrics {
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: 9px;
    }
    .metric {
      background: var(--panel-2);
      border: 1px solid var(--line);
      border-radius: 8px;
      padding: 10px;
      min-width: 0;
      box-shadow: inset 0 1px 0 rgba(255, 255, 255, .03);
    }
    .metric strong {
      display: block;
      font-size: 20px;
      line-height: 1;
      margin-bottom: 5px;
    }
    .metric span { color: var(--muted); font-size: 12px; }
    .legend {
      display: grid;
      gap: 8px;
      color: var(--muted);
    }
    .legend-row {
      display: flex;
      align-items: center;
      gap: 9px;
      min-width: 0;
    }
    .swatch {
      width: 13px;
      height: 13px;
      border-radius: 4px;
      flex: 0 0 auto;
      box-shadow: 0 0 14px currentColor;
    }
    .list {
      display: grid;
      gap: 8px;
    }
    .item {
      border: 1px solid var(--line);
      border-radius: 8px;
      padding: 10px;
      background: rgba(24, 30, 38, .9);
      min-width: 0;
      transition: border-color .16s ease, transform .16s ease, background .16s ease;
    }
    .item[data-node-id] { cursor: pointer; }
    .item[data-node-id]:hover,
    .item.selected {
      border-color: rgba(70, 166, 255, .68);
      background: var(--panel-3);
      transform: translateY(-1px);
    }
    .item-title {
      font-weight: 700;
      overflow-wrap: anywhere;
    }
    .item-meta {
      color: var(--muted);
      font-size: 12px;
      overflow-wrap: anywhere;
      margin-top: 2px;
    }
    .pill {
      display: inline-flex;
      align-items: center;
      min-height: 22px;
      padding: 2px 7px;
      border-radius: 999px;
      background: #222a34;
      border: 1px solid var(--line);
      color: var(--muted);
      font-size: 12px;
      margin: 7px 5px 0 0;
    }
    .pill.ok { color: var(--ok); border-color: rgba(31, 224, 161, .28); }
    .pill.warn { color: var(--warn); border-color: rgba(255, 208, 90, .28); }
    .pill.bad { color: var(--bad); border-color: rgba(255, 102, 115, .28); }
    .node {
      cursor: pointer;
      outline: none;
    }
    .node-halo {
      opacity: .16;
      filter: blur(.2px);
      transition: opacity .16s ease, r .16s ease;
    }
    .node-frame {
      stroke: rgba(255, 255, 255, .18);
      stroke-width: 1.5;
      filter: drop-shadow(0 12px 18px rgba(0, 0, 0, .35));
      transition: stroke .16s ease, stroke-width .16s ease, filter .16s ease;
    }
    .node-icon {
      fill: none;
      stroke: #071014;
      stroke-width: 2.2;
      stroke-linecap: round;
      stroke-linejoin: round;
      opacity: .95;
      pointer-events: none;
    }
    .node text {
      fill: var(--text);
      paint-order: stroke;
      stroke: #0d1117;
      stroke-width: 5;
      stroke-linejoin: round;
      font-size: 12px;
      font-weight: 720;
      pointer-events: none;
    }
    .node-subtext {
      fill: var(--muted) !important;
      font-size: 10px !important;
      font-weight: 640 !important;
      stroke-width: 4 !important;
    }
    .node:hover .node-halo,
    .node.selected .node-halo {
      opacity: .38;
    }
    .node:hover .node-frame,
    .node.selected .node-frame {
      stroke: rgba(255, 255, 255, .7);
      stroke-width: 2.4;
      filter: drop-shadow(0 0 18px rgba(70, 166, 255, .28));
    }
    .edge {
      stroke-width: 2.5;
      stroke: var(--line-strong);
      stroke-linecap: round;
      opacity: .82;
      transition: opacity .16s ease, stroke-width .16s ease;
    }
    .edge.enc {
      stroke: var(--ok);
      filter: drop-shadow(0 0 5px rgba(31, 224, 161, .22));
      stroke-dasharray: 10 7;
      animation: flow 1.8s linear infinite;
    }
    .edge.unenc {
      stroke: var(--warn);
      stroke-dasharray: 5 6;
      animation: flow 2.6s linear infinite;
    }
    .edge.room { stroke: var(--forward); }
    .edge.faded,
    .node.faded {
      opacity: .22;
    }
    .edge.selected {
      opacity: 1;
      stroke-width: 4;
    }
    .edge-label-pill {
      fill: rgba(14, 17, 22, .86);
      stroke: var(--line);
      stroke-width: 1;
    }
    .edge-label {
      fill: var(--muted);
      font-size: 10.5px;
      font-weight: 700;
      pointer-events: none;
    }
    .minimap-ring {
      fill: none;
      stroke: rgba(255, 255, 255, .07);
      stroke-width: 1;
    }
    @keyframes flow {
      to { stroke-dashoffset: -34; }
    }
    @media (prefers-reduced-motion: reduce) {
      .edge.enc,
      .edge.unenc {
        animation: none;
      }
      .item,
      .node-frame,
      .node-halo {
        transition: none;
      }
    }
    @media (max-width: 1020px) {
      main { grid-template-columns: 1fr; }
      aside {
        border-left: 0;
        border-top: 1px solid var(--line);
      }
      #map, #map-wrap { min-height: 58vh; }
      header {
        align-items: flex-start;
        flex-direction: column;
        gap: 10px;
      }
      .header-meta { flex-wrap: wrap; white-space: normal; }
    }
  </style>
</head>
<body>
  <div class="shell">
    <header>
      <div class="brand">
        <div class="brand-mark" aria-hidden="true">
          <svg viewBox="0 0 24 24" width="21" height="21">
            <path d="M4 8h5v5H4zM15 4h5v5h-5zM15 15h5v5h-5z" fill="none" stroke="#46a6ff" stroke-width="1.8" stroke-linejoin="round"/>
            <path d="M9 10.5h3.5V6.5H15M9 10.5h3.5V17.5H15" fill="none" stroke="#1fe0a1" stroke-width="1.8" stroke-linecap="round"/>
          </svg>
        </div>
        <div>
          <h1>Labyrinth Network Map</h1>
          <span>Read-only topology, tunnels, routes, dwellers, and forwards</span>
        </div>
      </div>
      <div class="header-meta">
        <span class="status-chip"><i id="status-dot" class="status-dot"></i><span id="status-text">Connecting</span></span>
        <span id="updated">Waiting for snapshot</span>
      </div>
    </header>
    <main>
      <section id="map-wrap" aria-label="Network visualization">
        <div class="map-toolbar">
          <span class="tool">
            <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><path d="M12 3v18M3 12h18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>
            Click nodes for details
          </span>
          <span class="tool" id="edge-count">0 links</span>
        </div>
        <svg id="map" role="img" aria-label="Labyrinth network topology"></svg>
        <div id="empty" class="empty">
          <div class="empty-panel">
            <div class="empty-title">No connected agents yet</div>
            <div>Start an agent or connect a dweller to populate the live topology.</div>
          </div>
        </div>
      </section>
      <aside>
        <section class="section">
          <h2>Summary</h2>
          <div class="metrics" id="metrics"></div>
        </section>
        <section class="section">
          <h2>Selected</h2>
          <div class="list" id="selected"></div>
        </section>
        <section class="section">
          <h2>Legend</h2>
          <div class="legend">
            <div class="legend-row"><span class="swatch" style="background:var(--accent); color:var(--accent)"></span>Server / proxy</div>
            <div class="legend-row"><span class="swatch" style="background:var(--agent); color:var(--agent)"></span>Agent</div>
            <div class="legend-row"><span class="swatch" style="background:var(--dweller); color:var(--dweller)"></span>Dweller</div>
            <div class="legend-row"><span class="swatch" style="background:var(--network); color:var(--network)"></span>Detected network</div>
            <div class="legend-row"><span class="swatch" style="background:var(--forward); color:var(--forward)"></span>Room port forward</div>
            <div class="legend-row"><span class="swatch" style="background:var(--ok); color:var(--ok)"></span>Encrypted edge</div>
            <div class="legend-row"><span class="swatch" style="background:var(--warn); color:var(--warn)"></span>Unencrypted/local edge</div>
          </div>
        </section>
        <section class="section">
          <h2>Agents And Dwellers</h2>
          <div class="list" id="agents"></div>
        </section>
        <section class="section">
          <h2>Detected Networks</h2>
          <div class="list" id="networks"></div>
        </section>
        <section class="section">
          <h2>Active Tunnels</h2>
          <div class="list" id="tunnels"></div>
        </section>
        <section class="section">
          <h2>Room Forwards</h2>
          <div class="list" id="forwards"></div>
        </section>
        <section class="section">
          <h2>Shared Networks</h2>
          <div class="list" id="shared"></div>
        </section>
        <section class="section">
          <h2>Route Conflicts</h2>
          <div class="list" id="conflicts"></div>
        </section>
      </aside>
    </main>
  </div>
  <script>
    const svg = document.getElementById('map');
    const empty = document.getElementById('empty');
    const colors = {
      server: '#46a6ff',
      agent: '#23d18b',
      dweller: '#d95cff',
      network: '#f6c945',
      port_forward: '#ff8b5c'
    };
    const icons = {
      server: [
        ['rect', { x: -13, y: -11, width: 26, height: 22, rx: 4 }],
        ['path', { d: 'M-8 -4h16M-8 4h16M6 -4h.1M6 4h.1' }]
      ],
      agent: [
        ['rect', { x: -13, y: -9, width: 26, height: 18, rx: 3 }],
        ['path', { d: 'M-6 14h12M-2 9v5M-7 -3h14' }]
      ],
      dweller: [
        ['path', { d: 'M-12 -3l12 -7 12 7v11l-12 7-12 -7z' }],
        ['path', { d: 'M-5 -1h10M-5 5h10M0 -10v25' }]
      ],
      network: [
        ['circle', { cx: 0, cy: 0, r: 11 }],
        ['path', { d: 'M-11 0h22M0 -11c4 4 4 18 0 22M0 -11c-4 4-4 18 0 22' }]
      ],
      port_forward: [
        ['path', { d: 'M-11 -7h10v14h-10zM1 -7h10v14H1zM-1 0h2M-7 0h2M5 0h2' }]
      ]
    };
    let current = { nodes: [], edges: [], routes: [], port_forwards: [], fullhouse: [], shared_networks: [], conflicts: [] };
    let selectedNodeId = '';

    function el(name, attrs = {}, parent = svg) {
      const node = document.createElementNS('http://www.w3.org/2000/svg', name);
      for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, value);
      parent.appendChild(node);
      return node;
    }

    function escapeHtml(value) {
      return String(value ?? '').replace(/[&<>"']/g, (char) => ({
        '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;'
      }[char]));
    }

    function layout(nodes, edges, width, height) {
      const center = { x: width * 0.44, y: height * 0.52 };
      const nodeMap = new Map();
      const byKind = {
        server: nodes.filter(n => n.kind === 'server'),
        agent: nodes.filter(n => n.kind === 'agent' || n.kind === 'dweller'),
        network: nodes.filter(n => n.kind === 'network'),
        port_forward: nodes.filter(n => n.kind === 'port_forward')
      };

      for (const node of nodes) {
        node.vx = 0;
        node.vy = 0;
      }
      for (const node of byKind.server) {
        node.x = center.x;
        node.y = center.y;
        nodeMap.set(node.id, node);
      }
      const rings = [
        [byKind.agent, Math.min(width, height) * 0.26, -Math.PI / 2],
        [byKind.network, Math.min(width, height) * 0.40, -Math.PI / 5],
        [byKind.port_forward, Math.min(width, height) * 0.34, Math.PI / 2]
      ];
      for (const [group, radius, offset] of rings) {
        group.forEach((node, index) => {
          const angle = offset + (Math.PI * 2 * index) / Math.max(group.length, 1);
          node.x = center.x + Math.cos(angle) * radius;
          node.y = center.y + Math.sin(angle) * radius;
          nodeMap.set(node.id, node);
        });
      }

      for (let tick = 0; tick < 160; tick++) {
        for (let i = 0; i < nodes.length; i++) {
          for (let j = i + 1; j < nodes.length; j++) {
            const a = nodes[i], b = nodes[j];
            let dx = a.x - b.x, dy = a.y - b.y;
            let dist = Math.max(Math.sqrt(dx * dx + dy * dy), 1);
            const force = 2300 / (dist * dist);
            dx /= dist; dy /= dist;
            if (a.kind !== 'server') { a.vx += dx * force; a.vy += dy * force; }
            if (b.kind !== 'server') { b.vx -= dx * force; b.vy -= dy * force; }
          }
        }
        for (const edge of edges) {
          const a = nodeMap.get(edge.source), b = nodeMap.get(edge.target);
          if (!a || !b) continue;
          let dx = b.x - a.x, dy = b.y - a.y;
          let dist = Math.max(Math.sqrt(dx * dx + dy * dy), 1);
          const desired = edge.kind === 'route' ? 156 : edge.kind === 'room' ? 130 : 190;
          const force = (dist - desired) * 0.013;
          dx /= dist; dy /= dist;
          if (a.kind !== 'server') { a.vx += dx * force; a.vy += dy * force; }
          if (b.kind !== 'server') { b.vx -= dx * force; b.vy -= dy * force; }
        }
        for (const node of nodes) {
          if (node.kind === 'server') {
            node.x = center.x; node.y = center.y; node.vx = 0; node.vy = 0;
            continue;
          }
          node.vx += (center.x - node.x) * 0.0015;
          node.vy += (center.y - node.y) * 0.0015;
          node.x = Math.max(76, Math.min(width - 76, node.x + node.vx));
          node.y = Math.max(76, Math.min(height - 76, node.y + node.vy));
          node.vx *= 0.85; node.vy *= 0.85;
        }
      }
      return nodeMap;
    }

    function nodeRadius(node) {
      if (node.kind === 'server') return 30;
      if (node.kind === 'network') return 23;
      if (node.kind === 'port_forward') return 22;
      return 25;
    }

    function edgeTouches(edge, nodeId) {
      return edge.source === nodeId || edge.target === nodeId;
    }

    function render(data) {
      current = data;
      if (selectedNodeId && !data.nodes.some(node => node.id === selectedNodeId)) selectedNodeId = '';
      const bounds = svg.getBoundingClientRect();
      const width = Math.max(bounds.width, 700);
      const height = Math.max(bounds.height, 440);
      svg.setAttribute('viewBox', `0 0 ${width} ${height}`);
      svg.innerHTML = '';
      empty.style.display = data.nodes.length <= 1 ? 'flex' : 'none';
      document.getElementById('edge-count').textContent = `${data.edges.length} links`;
      const nodes = data.nodes.map(node => ({ ...node }));
      const edges = data.edges.map(edge => ({ ...edge }));
      const nodeMap = layout(nodes, edges, width, height);

      const defs = el('defs');
      const glow = el('filter', { id: 'soft-glow', x: '-40%', y: '-40%', width: '180%', height: '180%' }, defs);
      el('feGaussianBlur', { stdDeviation: 4, result: 'blur' }, glow);
      const merge = el('feMerge', {}, glow);
      el('feMergeNode', { in: 'blur' }, merge);
      el('feMergeNode', { in: 'SourceGraphic' }, merge);

      const backgroundLayer = el('g');
      const edgeLayer = el('g');
      const labelLayer = el('g');
      const nodeLayer = el('g');
      const center = nodeMap.get('server');
      if (center) {
        [110, 210, 320].forEach(radius => el('circle', {
          cx: center.x,
          cy: center.y,
          r: radius,
          class: 'minimap-ring'
        }, backgroundLayer));
      }

      for (const edge of edges) {
        const a = nodeMap.get(edge.source), b = nodeMap.get(edge.target);
        if (!a || !b) continue;
        const isSelected = selectedNodeId && edgeTouches(edge, selectedNodeId);
        const faded = selectedNodeId && !isSelected && selectedNodeId !== '';
        const cls = `edge ${edge.encrypted ? 'enc' : 'unenc'} ${edge.kind} ${isSelected ? 'selected' : ''} ${faded ? 'faded' : ''}`;
        el('line', {
          x1: a.x, y1: a.y, x2: b.x, y2: b.y,
          class: cls
        }, edgeLayer);
        const labelT = edge.kind === 'route' ? 0.68 : 0.5;
        const midX = a.x + (b.x - a.x) * labelT;
        const midY = a.y + (b.y - a.y) * labelT;
        const labelWidth = Math.min(Math.max(edge.label.length * 6.4 + 16, 54), 138);
        el('rect', {
          x: midX - labelWidth / 2,
          y: midY - 19,
          width: labelWidth,
          height: 18,
          rx: 6,
          class: 'edge-label-pill'
        }, labelLayer);
        el('text', {
          x: midX,
          y: midY - 6,
          'text-anchor': 'middle',
          class: 'edge-label'
        }, labelLayer).textContent = edge.label;
      }
      for (const node of nodes) {
        const radius = nodeRadius(node);
        const color = colors[node.kind] || '#99a6b3';
        const connected = selectedNodeId && edges.some(edge => edgeTouches(edge, selectedNodeId) && edgeTouches(edge, node.id));
        const selected = node.id === selectedNodeId;
        const faded = selectedNodeId && !selected && !connected && selectedNodeId !== '';
        const group = el('g', {
          class: `node ${selected ? 'selected' : ''} ${faded ? 'faded' : ''}`,
          transform: `translate(${node.x},${node.y})`,
          tabindex: '0',
          role: 'button',
          'aria-label': `${node.label} ${node.status}`
        }, nodeLayer);
        group.addEventListener('click', () => selectNode(node.id));
        group.addEventListener('keydown', (event) => {
          if (event.key === 'Enter' || event.key === ' ') {
            event.preventDefault();
            selectNode(node.id);
          }
        });
        el('circle', { r: radius + 11, fill: color, class: 'node-halo', filter: 'url(#soft-glow)' }, group);
        el('circle', { r: radius, fill: color, class: 'node-frame' }, group);
        for (const [name, attrs] of icons[node.kind] || icons.agent) {
          el(name, { ...attrs, class: 'node-icon' }, group);
        }
        el('text', { x: 0, y: radius + 18, 'text-anchor': 'middle' }, group).textContent = shortLabel(node.label, 22);
        el('text', { x: 0, y: radius + 32, 'text-anchor': 'middle', class: 'node-subtext' }, group).textContent = shortLabel(node.status, 26);
        const title = el('title', {}, group);
        title.textContent = `${node.label}\n${node.status}\n${node.detail || ''}`;
      }
      renderSide(data);
    }

    function shortLabel(value, max) {
      const text = String(value || '');
      return text.length > max ? `${text.slice(0, max - 1)}…` : text;
    }

    function metric(label, value) {
      return `<div class="metric"><strong>${escapeHtml(value)}</strong><span>${escapeHtml(label)}</span></div>`;
    }

    function item(title, meta, pills = [], nodeId = '') {
      const selected = nodeId && nodeId === selectedNodeId ? ' selected' : '';
      const nodeAttr = nodeId ? ` data-node-id="${escapeHtml(nodeId)}"` : '';
      return `<div class="item${selected}"${nodeAttr}><div class="item-title">${escapeHtml(title)}</div><div class="item-meta">${escapeHtml(meta || '')}</div>${pills.map(p => `<span class="pill ${escapeHtml(p.tone || '')}">${escapeHtml(p.text)}</span>`).join('')}</div>`;
    }

    function emptyText(text) {
      return `<div class="item"><div class="item-meta">${escapeHtml(text)}</div></div>`;
    }

    function bindListSelection() {
      document.querySelectorAll('[data-node-id]').forEach(node => {
        node.addEventListener('click', () => selectNode(node.getAttribute('data-node-id')));
      });
    }

    function selectNode(nodeId) {
      selectedNodeId = nodeId || '';
      render(current);
    }

    function selectedDetails(data) {
      const selected = data.nodes.find(node => node.id === selectedNodeId) || data.nodes[0];
      if (!selectedNodeId || !selected) return emptyText('Click a node or list item to inspect its links.');
      const connectedEdges = data.edges.filter(edge => edgeTouches(edge, selected.id));
      return item(
        selected.label,
        selected.detail || selected.status,
        [
          { text: selected.kind.replace('_', ' ') },
          { text: selected.status, tone: selected.status.includes('offline') ? 'warn' : 'ok' },
          { text: `${connectedEdges.length} links` }
        ],
        selected.id
      );
    }

    function renderSide(data) {
      const s = data.summary;
      document.getElementById('metrics').innerHTML =
        metric('agents', s.agents_online) +
        metric('dwellers', `${s.dwellers_online}/${s.dwellers_total}`) +
        metric('networks', s.detected_networks) +
        metric('edges', data.edges.length);
      document.getElementById('selected').innerHTML = selectedDetails(data);
      document.getElementById('agents').innerHTML = data.nodes.filter(node => node.kind === 'agent' || node.kind === 'dweller')
        .map(node => item(node.label, node.detail, [{ text: node.kind }, { text: node.status, tone: node.status.includes('offline') ? 'warn' : 'ok' }], node.id)).join('') ||
        emptyText('No agents or dwellers connected.');
      document.getElementById('networks').innerHTML = data.routes.length
        ? data.routes.map(route => item(route.cidr, `${route.agent_name} via ${route.interface_name} (${route.source_address})`, [{ text: `score ${route.score}` }, { text: 'auto-detected', tone: 'ok' }], `network:${route.cidr}`)).join('')
        : emptyText('No CIDRs detected from agent interfaces.');
      document.getElementById('tunnels').innerHTML = data.fullhouse.length
        ? data.fullhouse.map(t => item(t.agent_name, `proxy:${t.proxy_port}`, [{ text: 'tun/tls/enc', tone: 'ok' }], `agent:${t.agent_id}`)).join('')
        : emptyText('No active Fullhouse tunnels.');
      document.getElementById('forwards').innerHTML = data.port_forwards.length
        ? data.port_forwards.map(f => item(`localhost:${f.local_port}`, `${f.agent_name} -> ${f.target_host}:${f.target_port}`, [{ text: 'local/unenc', tone: 'warn' }, { text: 'stream/tls/enc', tone: 'ok' }], `port-forward:${f.local_port}`)).join('')
        : emptyText('No active Room forwards.');
      document.getElementById('shared').innerHTML = data.shared_networks.length
        ? data.shared_networks.map(g => item(g.cidr, g.agents.join(', '), [{ text: 'multi-hop candidate', tone: 'ok' }], `network:${g.cidr}`)).join('')
        : emptyText('No shared agent networks detected.');
      document.getElementById('conflicts').innerHTML = data.conflicts.length
        ? data.conflicts.map(c => item(c.cidr, c.agents.join(', '), [{ text: 'overlap', tone: 'bad' }], `network:${c.cidr}`)).join('')
        : emptyText('No route ownership conflicts.');
      bindListSelection();
    }

    async function refresh() {
      try {
        const response = await fetch('/api/network-map', { cache: 'no-store' });
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        const data = await response.json();
        document.getElementById('status-dot').classList.add('online');
        document.getElementById('status-text').textContent = 'Connected';
        document.getElementById('updated').textContent = `Updated ${new Date(data.generated_at_unix * 1000).toLocaleTimeString()}`;
        render(data);
      } catch (err) {
        document.getElementById('status-dot').classList.remove('online');
        document.getElementById('status-text').textContent = 'Disconnected';
      }
    }

    window.addEventListener('resize', () => render(current));
    svg.addEventListener('click', (event) => {
      if (event.target === svg) selectNode('');
    });
    refresh();
    setInterval(refresh, 2000);
  </script>
</body>
</html>
"##;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct DashboardSnapshot {
    generated_at_unix: u64,
    summary: DashboardSummary,
    nodes: Vec<DashboardNode>,
    edges: Vec<DashboardEdge>,
    routes: Vec<DashboardRoute>,
    shared_networks: Vec<DashboardSharedNetwork>,
    conflicts: Vec<DashboardRouteConflict>,
    port_forwards: Vec<DashboardPortForward>,
    fullhouse: Vec<DashboardFullhouse>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DashboardSummary {
    agents_online: usize,
    dwellers_online: usize,
    dwellers_total: usize,
    detected_networks: usize,
    active_tunnels: usize,
    port_forwards: usize,
    route_conflicts: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DashboardNode {
    id: String,
    label: String,
    kind: String,
    status: String,
    detail: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DashboardEdge {
    source: String,
    target: String,
    label: String,
    encrypted: bool,
    kind: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DashboardRoute {
    agent_id: String,
    agent_name: String,
    cidr: String,
    interface_name: String,
    source_address: String,
    score: u16,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DashboardSharedNetwork {
    cidr: String,
    agents: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DashboardRouteConflict {
    cidr: String,
    agents: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DashboardPortForward {
    local_port: u16,
    agent_id: String,
    agent_name: String,
    target_host: String,
    target_port: u16,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DashboardFullhouse {
    agent_id: String,
    agent_name: String,
    proxy_port: u16,
}

pub(crate) struct DashboardServer;

impl DashboardServer {
    pub(crate) async fn spawn(server: Arc<LabyrinthServer>, listen_addr: &str) -> Result<()> {
        let listener = TcpListener::bind(listen_addr).await?;
        let local_addr = listener.local_addr()?;
        let url = dashboard_url(local_addr);
        println!("{} Web UI: {}", "[+]".green().bold(), url.cyan());
        info!("Dashboard listening on {}", url);

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _peer)) => {
                        let server = Arc::clone(&server);
                        tokio::spawn(async move {
                            if let Err(e) = handle_dashboard_connection(stream, server).await {
                                error!("Dashboard request failed: {}", e);
                            }
                        });
                    }
                    Err(e) => error!("Dashboard accept failed: {}", e),
                }
            }
        });

        Ok(())
    }

    pub(crate) async fn snapshot(server: &LabyrinthServer) -> DashboardSnapshot {
        let port_forwards = server.port_forward_snapshots().await;
        let fullhouse = server.fullhouse_snapshots().await;
        let agents = server.agents().read().await;
        let dwellers = server.dweller_registry().read().await;
        Self::build_snapshot(&agents, &dwellers, &port_forwards, &fullhouse).await
    }

    async fn build_snapshot(
        agents: &HashMap<String, ConnectedAgent>,
        dwellers: &DwellerRegistry,
        port_forwards: &[PortForwardSnapshot],
        fullhouse: &[FullhouseSnapshot],
    ) -> DashboardSnapshot {
        let topology = TopologyManager::build_snapshot(agents);
        let mut nodes = vec![DashboardNode {
            id: "server".to_string(),
            label: "Server".to_string(),
            kind: "server".to_string(),
            status: "online".to_string(),
            detail: "Labyrinth proxy/control server".to_string(),
        }];
        let mut edges = Vec::new();

        let mut sorted_agents: Vec<_> = agents.values().collect();
        sorted_agents.sort_by(|left, right| {
            left.info
                .name
                .cmp(&right.info.name)
                .then_with(|| left.id.cmp(&right.id))
        });

        for agent in sorted_agents {
            let agent_node_id = agent_node_id(&agent.id);
            nodes.push(agent_dashboard_node(agent).await);
            edges.push(DashboardEdge {
                source: "server".to_string(),
                target: agent_node_id.clone(),
                label: "tls/enc".to_string(),
                encrypted: true,
                kind: "transport".to_string(),
            });

            if agent.tunnel_active {
                edges.push(DashboardEdge {
                    source: "server".to_string(),
                    target: agent_node_id.clone(),
                    label: active_transport_label(agent),
                    encrypted: true,
                    kind: "tunnel".to_string(),
                });
            }
        }

        append_route_nodes(&topology, &mut nodes, &mut edges);
        append_port_forward_nodes(agents, port_forwards, &mut nodes, &mut edges);
        append_offline_dweller_nodes(agents, dwellers, &mut nodes, &mut edges);

        let dwellers_online = agents
            .values()
            .filter(|agent| matches!(agent.info.kind, AgentKind::Dweller))
            .count();

        DashboardSnapshot {
            generated_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            summary: DashboardSummary {
                agents_online: agents.len(),
                dwellers_online,
                dwellers_total: dwellers.dwellers.len(),
                detected_networks: topology.routes.len(),
                active_tunnels: fullhouse.len()
                    + agents
                        .values()
                        .filter(|agent| agent.tunnel_active && !is_room_transport(agent))
                        .count(),
                port_forwards: port_forwards.len(),
                route_conflicts: topology.conflicts.len(),
            },
            routes: topology
                .routes
                .into_iter()
                .map(|route| DashboardRoute {
                    agent_id: route.agent_id,
                    agent_name: route.agent_name,
                    cidr: route.cidr,
                    interface_name: route.interface_name,
                    source_address: route.source_address,
                    score: route.score,
                })
                .collect(),
            shared_networks: topology
                .shared_routes
                .into_iter()
                .map(|group| DashboardSharedNetwork {
                    cidr: group.cidr,
                    agents: group.agents,
                })
                .collect(),
            conflicts: topology
                .conflicts
                .into_iter()
                .map(|conflict| DashboardRouteConflict {
                    cidr: conflict.cidr,
                    agents: conflict.agents,
                })
                .collect(),
            port_forwards: port_forwards
                .iter()
                .map(|forward| DashboardPortForward {
                    local_port: forward.local_port,
                    agent_id: forward.agent_id.clone(),
                    agent_name: agent_name(agents, &forward.agent_id),
                    target_host: forward.target_host.clone(),
                    target_port: forward.target_port,
                })
                .collect(),
            fullhouse: fullhouse
                .iter()
                .map(|snapshot| DashboardFullhouse {
                    agent_id: snapshot.agent_id.clone(),
                    agent_name: agent_name(agents, &snapshot.agent_id),
                    proxy_port: snapshot.proxy_port,
                })
                .collect(),
            nodes,
            edges,
        }
    }
}

async fn handle_dashboard_connection(
    mut stream: TcpStream,
    server: Arc<LabyrinthServer>,
) -> Result<()> {
    let request = read_http_request(&mut stream).await?;
    let Some((method, path)) = parse_request_line(&request) else {
        write_response(
            &mut stream,
            400,
            "text/plain; charset=utf-8",
            b"Bad Request",
        )
        .await?;
        return Ok(());
    };

    if method != "GET" && method != "HEAD" {
        write_response(
            &mut stream,
            405,
            "text/plain; charset=utf-8",
            b"Method Not Allowed",
        )
        .await?;
        return Ok(());
    }

    let body = match path {
        "/" | "/index.html" => ResponseBody::Static("text/html; charset=utf-8", DASHBOARD_HTML),
        "/health" => ResponseBody::Static("text/plain; charset=utf-8", "ok\n"),
        "/api/network-map" => {
            let snapshot = DashboardServer::snapshot(&server).await;
            let json = serde_json::to_string(&snapshot)?;
            ResponseBody::Owned("application/json; charset=utf-8", json)
        }
        _ => ResponseBody::Static("text/plain; charset=utf-8", "Not Found"),
    };

    let status = if matches!(path, "/" | "/index.html" | "/health" | "/api/network-map") {
        200
    } else {
        404
    };

    if method == "HEAD" {
        write_response(&mut stream, status, body.content_type(), b"").await?;
    } else {
        write_response(&mut stream, status, body.content_type(), body.bytes()).await?;
    }
    Ok(())
}

enum ResponseBody {
    Static(&'static str, &'static str),
    Owned(&'static str, String),
}

impl ResponseBody {
    fn content_type(&self) -> &'static str {
        match self {
            Self::Static(content_type, _) | Self::Owned(content_type, _) => content_type,
        }
    }

    fn bytes(&self) -> &[u8] {
        match self {
            Self::Static(_, body) => body.as_bytes(),
            Self::Owned(_, body) => body.as_bytes(),
        }
    }
}

async fn read_http_request(stream: &mut TcpStream) -> Result<String> {
    let mut buffer = vec![0_u8; 8192];
    let read = timeout(Duration::from_secs(5), stream.read(&mut buffer))
        .await
        .map_err(|_| LabyrinthError::Message("dashboard request timed out".to_string()))??;
    Ok(String::from_utf8_lossy(&buffer[..read]).to_string())
}

fn parse_request_line(request: &str) -> Option<(&str, &str)> {
    let line = request.lines().next()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?.split('?').next()?;
    Some((method, path))
}

async fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    let headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        status,
        reason,
        content_type,
        body.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await?;
    Ok(())
}

fn append_route_nodes(
    topology: &TopologySnapshot,
    nodes: &mut Vec<DashboardNode>,
    edges: &mut Vec<DashboardEdge>,
) {
    let mut seen_networks = std::collections::BTreeSet::new();
    for route in &topology.routes {
        let network_id = network_node_id(&route.cidr);
        if seen_networks.insert(route.cidr.clone()) {
            nodes.push(DashboardNode {
                id: network_id.clone(),
                label: route.cidr.clone(),
                kind: "network".to_string(),
                status: "detected".to_string(),
                detail: "CIDR inferred from agent interfaces".to_string(),
            });
        }
        edges.push(DashboardEdge {
            source: agent_node_id(&route.agent_id),
            target: network_id,
            label: format!("{} route", route.interface_name),
            encrypted: true,
            kind: "route".to_string(),
        });
    }
}

fn append_port_forward_nodes(
    agents: &HashMap<String, ConnectedAgent>,
    port_forwards: &[PortForwardSnapshot],
    nodes: &mut Vec<DashboardNode>,
    edges: &mut Vec<DashboardEdge>,
) {
    for forward in port_forwards {
        let forward_id = format!("port-forward:{}", forward.local_port);
        nodes.push(DashboardNode {
            id: forward_id.clone(),
            label: format!("PF :{}", forward.local_port),
            kind: "port_forward".to_string(),
            status: "active".to_string(),
            detail: format!("{}:{}", forward.target_host, forward.target_port),
        });
        edges.push(DashboardEdge {
            source: "server".to_string(),
            target: forward_id.clone(),
            label: "local/unenc".to_string(),
            encrypted: false,
            kind: "local_listener".to_string(),
        });
        edges.push(DashboardEdge {
            source: forward_id,
            target: agent_node_id(&forward.agent_id),
            label: "stream/tls/enc".to_string(),
            encrypted: true,
            kind: "room".to_string(),
        });
        if !agents.contains_key(&forward.agent_id) {
            edges.push(DashboardEdge {
                source: "server".to_string(),
                target: agent_node_id(&forward.agent_id),
                label: "stale owner".to_string(),
                encrypted: false,
                kind: "stale".to_string(),
            });
        }
    }
}

fn append_offline_dweller_nodes(
    agents: &HashMap<String, ConnectedAgent>,
    dwellers: &DwellerRegistry,
    nodes: &mut Vec<DashboardNode>,
    edges: &mut Vec<DashboardEdge>,
) {
    let online: std::collections::HashSet<&str> = agents.keys().map(String::as_str).collect();
    let mut records: Vec<&DwellerRecord> = dwellers.list();
    records.sort_by(|left, right| {
        left.dweller_name
            .cmp(&right.dweller_name)
            .then_with(|| left.dweller_id.cmp(&right.dweller_id))
    });
    for record in records {
        if online.contains(record.dweller_id.as_str()) {
            continue;
        }
        let node_id = agent_node_id(&record.dweller_id);
        nodes.push(DashboardNode {
            id: node_id.clone(),
            label: record.dweller_name.clone(),
            kind: "dweller".to_string(),
            status: "remembered/offline".to_string(),
            detail: format!("{} {}", record.socket_addr(), record.os),
        });
        edges.push(DashboardEdge {
            source: "server".to_string(),
            target: node_id,
            label: "remembered".to_string(),
            encrypted: false,
            kind: "registry".to_string(),
        });
    }
}

async fn agent_dashboard_node(agent: &ConnectedAgent) -> DashboardNode {
    let elapsed = agent.last_seen.lock().await.elapsed().as_secs();
    let kind = match agent.info.kind {
        AgentKind::Generic => "agent",
        AgentKind::Dweller => "dweller",
    };
    let status = if agent.tunnel_active {
        agent
            .tunnel_subnet
            .as_deref()
            .unwrap_or("transport active")
            .to_string()
    } else {
        "online".to_string()
    };
    DashboardNode {
        id: agent_node_id(&agent.id),
        label: agent.info.name.clone(),
        kind: kind.to_string(),
        status,
        detail: format!(
            "{} / {} / last seen {}s ago",
            agent.info.hostname, agent.info.os, elapsed
        ),
    }
}

fn dashboard_url(addr: SocketAddr) -> String {
    if addr.ip().is_ipv6() {
        format!("http://[{}]:{}", addr.ip(), addr.port())
    } else {
        format!("http://{}:{}", addr.ip(), addr.port())
    }
}

fn agent_node_id(agent_id: &str) -> String {
    format!("agent:{}", agent_id)
}

fn network_node_id(cidr: &str) -> String {
    format!("network:{}", cidr)
}

fn agent_name(agents: &HashMap<String, ConnectedAgent>, agent_id: &str) -> String {
    agents
        .get(agent_id)
        .map(|agent| agent.info.name.clone())
        .unwrap_or_else(|| agent_id.to_string())
}

fn active_transport_label(agent: &ConnectedAgent) -> String {
    if is_room_transport(agent) {
        "room/tls/enc".to_string()
    } else {
        "tun/tls/enc".to_string()
    }
}

fn is_room_transport(agent: &ConnectedAgent) -> bool {
    agent
        .tunnel_subnet
        .as_deref()
        .is_some_and(|label| label.starts_with("Port forwarding:"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{AgentInfo, NetworkInterface};
    use std::time::Instant;
    use tokio::sync::{mpsc, Mutex};

    fn test_agent(id: &str, name: &str, kind: AgentKind, addresses: Vec<&str>) -> ConnectedAgent {
        let (sender, _rx) = mpsc::channel(1);
        ConnectedAgent {
            id: id.to_string(),
            info: AgentInfo {
                name: name.to_string(),
                hostname: name.to_string(),
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
                interfaces: vec![NetworkInterface {
                    name: "eth0".to_string(),
                    addresses: addresses.into_iter().map(str::to_string).collect(),
                    hardware_addr: "00:11:22:33:44:55".to_string(),
                    mtu: 1500,
                    flags: vec!["UP".to_string(), "LOWER_UP".to_string()],
                }],
                auth_key: None,
                kind,
                stable_id: None,
                listener_addr: None,
                listener_port: None,
            },
            sender,
            tunnel_active: false,
            tunnel_subnet: None,
            tun_name: None,
            last_seen: Arc::new(Mutex::new(Instant::now())),
            command_response: Arc::new(Mutex::new(None)),
            shell_events: Arc::new(Mutex::new(None)),
        }
    }

    #[tokio::test]
    async fn snapshot_includes_agent_route_and_encryption_labels() {
        let mut agents = HashMap::new();
        agents.insert(
            "agent-a".to_string(),
            test_agent("agent-a", "alpha", AgentKind::Generic, vec!["10.10.1.4/24"]),
        );

        let snapshot = DashboardServer::build_snapshot(
            &agents,
            &DwellerRegistry::default(),
            &[PortForwardSnapshot {
                local_port: 8080,
                agent_id: "agent-a".to_string(),
                target_host: "10.10.1.20".to_string(),
                target_port: 80,
            }],
            &[],
        )
        .await;

        assert_eq!(snapshot.summary.agents_online, 1);
        assert!(snapshot
            .nodes
            .iter()
            .any(|node| node.kind == "network" && node.label == "10.10.1.0/24"));
        assert!(snapshot
            .edges
            .iter()
            .any(|edge| edge.label == "tls/enc" && edge.encrypted));
        assert!(snapshot
            .edges
            .iter()
            .any(|edge| edge.label == "local/unenc" && !edge.encrypted));
    }

    #[test]
    fn parses_http_request_line_with_query() {
        let request = "GET /api/network-map?poll=1 HTTP/1.1\r\nHost: localhost\r\n\r\n";
        assert_eq!(
            parse_request_line(request),
            Some(("GET", "/api/network-map"))
        );
    }

    #[test]
    fn dashboard_html_contains_visualization_mounts() {
        assert!(DASHBOARD_HTML.contains("Labyrinth Network Map"));
        assert!(DASHBOARD_HTML.contains("/api/network-map"));
        assert!(DASHBOARD_HTML.contains("<svg id=\"map\""));
    }
}
