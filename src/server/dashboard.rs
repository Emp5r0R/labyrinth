use crate::error::{LabyrinthError, Result};
use crate::protocol::{AgentKind, InternetAccess};
use crate::server::chain_manager::{ChainManager, ChainPlan};
use crate::server::core::{AriadneSnapshot, ConnectedAgent, LabyrinthServer, PortalSnapshot};
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
  <link rel="preconnect" href="https://fonts.googleapis.com">
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
  <link href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&family=Outfit:wght@500;600;700;800&display=swap" rel="stylesheet">
  <style>
    :root {
      color-scheme: dark;
      --bg: #030712;
      --panel: rgba(15, 23, 42, 0.65);
      --panel-solid: #0d121f;
      --panel-hover: rgba(30, 41, 59, 0.8);
      --panel-active: rgba(59, 130, 246, 0.08);
      --border: rgba(255, 255, 255, 0.06);
      --border-strong: rgba(255, 255, 255, 0.15);
      --border-hover: rgba(255, 255, 255, 0.2);
      --text: #f3f4f6;
      --muted: #9ca3af;
      --faint: #4b5563;

      --accent: #3b82f6;
      --server: #3b82f6;
      --agent: #10b981;
      --dweller: #a855f7;
      --network: #f59e0b;
      --forward: #f97316;

      --ok: #10b981;
      --warn: #f59e0b;
      --bad: #ef4444;
      --shadow: rgba(0, 0, 0, 0.5);
    }

    * {
      box-sizing: border-box;
      scrollbar-width: thin;
      scrollbar-color: rgba(255, 255, 255, 0.1) transparent;
    }

    *::-webkit-scrollbar {
      width: 6px;
      height: 6px;
    }
    *::-webkit-scrollbar-track {
      background: transparent;
    }
    *::-webkit-scrollbar-thumb {
      background: rgba(255, 255, 255, 0.15);
      border-radius: 4px;
    }
    *::-webkit-scrollbar-thumb:hover {
      background: rgba(255, 255, 255, 0.3);
    }

    body {
      margin: 0;
      min-height: 100vh;
      background-color: var(--bg);
      background-image: radial-gradient(circle at 50% 50%, #0f172a 0%, #030712 100%);
      color: var(--text);
      font-family: 'Inter', ui-sans-serif, system-ui, sans-serif;
      line-height: 1.5;
      overflow: hidden;
    }

    .shell {
      height: 100vh;
      display: grid;
      grid-template-rows: auto 1fr;
      overflow: hidden;
    }

    header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 20px;
      padding: 12px 24px;
      border-bottom: 1px solid var(--border);
      background: rgba(15, 23, 42, 0.8);
      backdrop-filter: blur(16px);
      -webkit-backdrop-filter: blur(16px);
      z-index: 10;
      box-shadow: 0 4px 30px rgba(0, 0, 0, 0.4);
    }

    .brand {
      display: flex;
      align-items: center;
      gap: 12px;
    }

    .brand-mark {
      width: 36px;
      height: 36px;
      border-radius: 8px;
      display: grid;
      place-items: center;
      background: linear-gradient(135deg, rgba(59, 130, 246, 0.2), rgba(16, 185, 129, 0.1));
      border: 1px solid rgba(255, 255, 255, 0.1);
      box-shadow: inset 0 0 10px rgba(59, 130, 246, 0.1);
      flex-shrink: 0;
    }

    .brand h1 {
      margin: 0;
      font-family: 'Outfit', sans-serif;
      font-size: 18px;
      font-weight: 700;
      letter-spacing: -0.02em;
      background: linear-gradient(90deg, #60a5fa, #34d399);
      -webkit-background-clip: text;
      -webkit-text-fill-color: transparent;
    }

    .brand span {
      display: block;
      color: var(--muted);
      font-size: 11px;
      margin-top: 1px;
    }

    .header-meta {
      display: flex;
      align-items: center;
      gap: 14px;
      font-size: 12.5px;
    }

    .status-chip {
      display: inline-flex;
      align-items: center;
      gap: 8px;
      height: 28px;
      padding: 0 12px;
      border: 1px solid var(--border);
      border-radius: 999px;
      background: rgba(0, 0, 0, 0.25);
    }

    .status-dot {
      width: 8px;
      height: 8px;
      border-radius: 50%;
      background: var(--bad);
      box-shadow: 0 0 8px var(--bad);
      display: inline-block;
    }

    .status-dot.online {
      background: var(--ok);
      box-shadow: 0 0 10px var(--ok);
      animation: pulse 2.5s infinite;
    }

    @keyframes pulse {
      0% { transform: scale(0.95); box-shadow: 0 0 0 0 rgba(16, 185, 129, 0.4); }
      70% { transform: scale(1); box-shadow: 0 0 0 6px rgba(16, 185, 129, 0); }
      100% { transform: scale(0.95); box-shadow: 0 0 0 0 rgba(16, 185, 129, 0); }
    }

    main {
      display: grid;
      grid-template-columns: 1fr 420px;
      height: calc(100vh - 61px);
      overflow: hidden;
    }

    #map-wrap {
      position: relative;
      height: 100%;
      overflow: hidden;
      background:
        radial-gradient(circle at 40% 45%, rgba(59, 130, 246, 0.04) 0%, transparent 60%),
        linear-gradient(rgba(255, 255, 255, 0.015) 1px, transparent 1px),
        linear-gradient(90deg, rgba(255, 255, 255, 0.015) 1px, transparent 1px);
      background-size: auto, 34px 34px, 34px 34px;
      cursor: grab;
      user-select: none;
    }

    #map-wrap:active {
      cursor: grabbing;
    }

    #map {
      width: 100%;
      height: 100%;
      display: block;
    }

    .map-toolbar {
      position: absolute;
      left: 18px;
      top: 18px;
      display: flex;
      flex-direction: column;
      gap: 8px;
      z-index: 5;
      pointer-events: none;
    }

    .map-toolbar > * {
      pointer-events: auto;
    }

    .map-search-bar {
      display: flex;
      align-items: center;
      gap: 8px;
      padding: 4px 12px;
      border-radius: 8px;
      background: rgba(15, 23, 42, 0.85);
      backdrop-filter: blur(12px);
      -webkit-backdrop-filter: blur(12px);
      border: 1px solid var(--border);
      box-shadow: 0 4px 15px rgba(0, 0, 0, 0.4);
    }

    .map-search-bar input {
      background: transparent;
      border: none;
      color: var(--text);
      font-family: inherit;
      font-size: 12px;
      outline: none;
      width: 160px;
    }

    .map-search-bar input::placeholder {
      color: var(--faint);
    }

    .map-controls-row {
      display: flex;
      gap: 6px;
    }

    .tool {
      height: 30px;
      display: inline-flex;
      align-items: center;
      gap: 6px;
      border: 1px solid var(--border);
      border-radius: 6px;
      padding: 0 10px;
      color: var(--muted);
      background: rgba(15, 23, 42, 0.85);
      backdrop-filter: blur(12px);
      -webkit-backdrop-filter: blur(12px);
      font-size: 11.5px;
      box-shadow: 0 4px 15px rgba(0, 0, 0, 0.3);
    }

    .btn-tool {
      cursor: pointer;
      user-select: none;
      font-weight: 600;
      transition: all 0.15s ease;
    }

    .btn-tool:hover {
      border-color: var(--border-hover);
      color: var(--text);
      background: var(--panel-hover);
    }

    .btn-tool:active {
      transform: translateY(1px);
    }

    aside {
      height: 100%;
      display: grid;
      grid-template-rows: auto 1fr;
      border-left: 1px solid var(--border);
      background: rgba(13, 18, 30, 0.8);
      backdrop-filter: blur(24px);
      -webkit-backdrop-filter: blur(24px);
      overflow: hidden;
      box-shadow: -4px 0 30px rgba(0, 0, 0, 0.25);
    }

    .sidebar-tabs {
      display: flex;
      border-bottom: 1px solid var(--border);
      padding: 8px 12px 0;
      gap: 4px;
      background: rgba(10, 15, 26, 0.4);
    }

    .tab-btn {
      background: transparent;
      border: none;
      border-bottom: 2px solid transparent;
      color: var(--muted);
      padding: 8px 14px;
      font-family: 'Outfit', sans-serif;
      font-size: 13px;
      font-weight: 600;
      cursor: pointer;
      display: inline-flex;
      align-items: center;
      gap: 6px;
      transition: all 0.2s ease;
      border-top-left-radius: 6px;
      border-top-right-radius: 6px;
    }

    .tab-btn:hover {
      color: var(--text);
      background: rgba(255, 255, 255, 0.02);
    }

    .tab-btn.active {
      color: var(--accent);
      border-bottom-color: var(--accent);
      background: var(--panel-active);
    }

    .tab-btn svg {
      opacity: 0.65;
    }

    .tab-btn.active svg {
      opacity: 1;
    }

    .sidebar-content {
      overflow-y: auto;
      padding: 16px;
    }

    .tab-pane {
      display: none;
    }

    .tab-pane.active {
      display: block;
    }

    .metrics-grid {
      display: grid;
      grid-template-columns: repeat(2, 1fr);
      gap: 10px;
      margin-bottom: 16px;
    }

    .glass {
      background: var(--panel);
      backdrop-filter: blur(12px);
      -webkit-backdrop-filter: blur(12px);
      border: 1px solid var(--border);
      box-shadow: 0 4px 24px rgba(0, 0, 0, 0.3);
    }

    .metric-card {
      padding: 12px;
      border-radius: 8px;
      display: flex;
      flex-direction: column;
      position: relative;
      overflow: hidden;
    }

    .metric-card::before {
      content: '';
      position: absolute;
      top: 0;
      left: 0;
      width: 3px;
      height: 100%;
      background: var(--card-color, var(--accent));
    }

    .metric-card h3 {
      margin: 0;
      font-size: 10.5px;
      text-transform: uppercase;
      color: var(--muted);
      letter-spacing: 0.04em;
      font-weight: 600;
    }

    .metric-card .val {
      font-family: 'Outfit', sans-serif;
      font-size: 20px;
      font-weight: 700;
      line-height: 1.2;
      margin-top: 4px;
      color: var(--text);
    }

    .inspect-card {
      border-radius: 8px;
      padding: 14px;
      margin-bottom: 16px;
      position: relative;
    }

    .inspect-card::before {
      content: '';
      position: absolute;
      top: 0;
      left: 0;
      width: 3px;
      height: 100%;
      background: var(--card-color, var(--accent));
    }

    .inspect-title {
      font-family: 'Outfit', sans-serif;
      font-size: 15px;
      font-weight: 700;
      display: flex;
      align-items: center;
      justify-content: space-between;
      margin-bottom: 10px;
    }

    .inspect-badge {
      font-size: 9.5px;
      padding: 1px 6px;
      border-radius: 99px;
      font-weight: 600;
      text-transform: uppercase;
      letter-spacing: 0.04em;
    }

    .inspect-prop-table {
      width: 100%;
      border-collapse: collapse;
      font-size: 12px;
    }

    .inspect-prop-table td {
      padding: 5px 0;
      vertical-align: top;
    }

    .inspect-prop-table td:first-child {
      color: var(--muted);
      width: 80px;
      font-weight: 500;
    }

    .inspect-prop-table td:last-child {
      word-break: break-all;
      font-weight: 600;
    }

    .section-title {
      font-family: 'Outfit', sans-serif;
      font-size: 12px;
      text-transform: uppercase;
      letter-spacing: 0.06em;
      color: var(--muted);
      margin: 16px 0 8px;
      display: flex;
      align-items: center;
      font-weight: 700;
    }

    .section-title::after {
      content: '';
      flex-grow: 1;
      height: 1px;
      background: var(--border);
      margin-left: 10px;
    }

    .list-container {
      display: grid;
      gap: 6px;
    }

    .list-item {
      border-radius: 8px;
      padding: 10px;
      transition: all 0.15s ease;
      background: rgba(255, 255, 255, 0.015);
      border: 1px solid var(--border);
    }

    .list-item:hover, .list-item.selected {
      background: var(--panel-hover);
      border-color: rgba(59, 130, 246, 0.35);
      transform: translateY(-1px);
    }

    .list-item.selected {
      box-shadow: inset 0 0 8px rgba(59, 130, 246, 0.08);
      border-color: rgba(59, 130, 246, 0.5);
    }

    .item-header {
      display: flex;
      justify-content: space-between;
      align-items: center;
      font-weight: 700;
      font-size: 13px;
    }

    .item-desc {
      font-size: 11.5px;
      color: var(--muted);
      margin-top: 2px;
      word-break: break-all;
    }

    .item-pills {
      display: flex;
      flex-wrap: wrap;
      gap: 4px;
      margin-top: 6px;
    }

    .pill-badge {
      font-size: 10.5px;
      padding: 1px 6px;
      border-radius: 4px;
      border: 1px solid rgba(255, 255, 255, 0.06);
      font-weight: 500;
      background: rgba(255, 255, 255, 0.03);
    }

    .pill-badge.success { color: var(--ok); border-color: rgba(16, 185, 129, 0.2); background: rgba(16, 185, 129, 0.04); }
    .pill-badge.warning { color: var(--warn); border-color: rgba(245, 158, 11, 0.2); background: rgba(245, 158, 11, 0.04); }
    .pill-badge.danger { color: var(--bad); border-color: rgba(239, 68, 68, 0.2); background: rgba(239, 68, 68, 0.04); }

    /* SVG Graph styling */
    .edge {
      fill: none;
      stroke-width: 2.2;
      transition: opacity 0.25s ease, stroke-width 0.25s ease;
      stroke: var(--faint);
      opacity: 0.35;
    }

    .edge.active-route {
      stroke-width: 3.5;
      opacity: 1;
    }

    .edge.encrypted {
      stroke: var(--ok);
      stroke-dasharray: 8 6;
      animation: flow 1.8s linear infinite;
      opacity: 0.85;
    }

    .edge.unencrypted {
      stroke: var(--warn);
      stroke-dasharray: 6 6;
      animation: flow 2.4s linear infinite;
      opacity: 0.85;
    }

    .edge.portal-forward {
      stroke: var(--forward);
      opacity: 0.8;
    }

    .edge.highlight-path {
      stroke: #60a5fa;
      stroke-width: 4.2;
      opacity: 1;
      filter: drop-shadow(0 0 6px rgba(96, 165, 250, 0.5));
      stroke-dasharray: 10 5;
      animation: flow 1.2s linear infinite;
    }

    @keyframes flow {
      to { stroke-dashoffset: -30; }
    }

    .node {
      cursor: grab;
      transition: opacity 0.25s ease;
    }

    .node:active {
      cursor: grabbing;
    }

    .node-halo {
      opacity: 0.06;
      transition: all 0.25s ease;
    }

    .node:hover .node-halo, .node.selected .node-halo {
      opacity: 0.25;
    }

    .node-frame {
      stroke: rgba(255, 255, 255, 0.15);
      stroke-width: 1.5;
      transition: all 0.25s ease;
    }

    .node:hover .node-frame, .node.selected .node-frame {
      stroke: rgba(255, 255, 255, 0.5);
      stroke-width: 2.2;
    }

    .node.selected .node-frame {
      stroke: #ffffff;
      filter: drop-shadow(0 0 10px var(--node-color));
    }

    .node.pinned .node-frame {
      stroke: #ffffff;
      stroke-width: 2px;
      stroke-dasharray: 4 2;
    }

    .node-icon {
      stroke: #ffffff;
      stroke-width: 1.8;
      stroke-linecap: round;
      stroke-linejoin: round;
      pointer-events: none;
    }

    .node-icon:not([fill]) {
      fill: none;
    }

    .node text {
      fill: var(--text);
      font-size: 11px;
      font-weight: 600;
      font-family: 'Outfit', sans-serif;
      pointer-events: none;
      paint-order: stroke;
      stroke: #030712;
      stroke-width: 3.5;
    }

    .node .subtext {
      fill: var(--muted);
      font-size: 9.5px;
      font-weight: 500;
      font-family: 'Inter', sans-serif;
      stroke-width: 3;
    }

    .node.filtered-out, .edge.filtered-out {
      opacity: 0.04;
      pointer-events: none;
    }

    .node.highlighted .node-halo {
      opacity: 0.45 !important;
      animation: pulse-halo 1.5s infinite;
    }

    @keyframes pulse-halo {
      0% { transform: scale(1); opacity: 0.45; }
      50% { transform: scale(1.15); opacity: 0.28; }
      100% { transform: scale(1); opacity: 0.45; }
    }

    .node.highlighted .node-frame {
      stroke: #ffffff !important;
      stroke-width: 2.8px !important;
      filter: drop-shadow(0 0 12px #60a5fa);
    }

    .empty-state {
      display: flex;
      flex-direction: column;
      align-items: center;
      justify-content: center;
      color: var(--muted);
      text-align: center;
      padding: 30px 16px;
      border-radius: 8px;
      border: 1.5px dashed var(--border);
      font-size: 12.5px;
    }

    .legend-card {
      padding: 12px;
      border-radius: 8px;
      margin-top: 12px;
    }

    .legend-grid {
      display: grid;
      gap: 8px;
      font-size: 11.5px;
    }

    .legend-item {
      display: flex;
      align-items: center;
      gap: 8px;
    }

    .legend-color {
      width: 12px;
      height: 12px;
      border-radius: 3px;
      box-shadow: 0 0 6px var(--legend-c);
      flex-shrink: 0;
    }

    .plan-actions-list {
      display: flex;
      flex-direction: column;
      gap: 8px;
      margin-top: 8px;
      position: relative;
      padding-left: 12px;
    }

    .plan-actions-list::before {
      content: '';
      position: absolute;
      left: 3px;
      top: 5px;
      bottom: 5px;
      width: 2px;
      background: var(--border-strong);
    }

    .plan-action-step {
      position: relative;
    }

    .plan-action-step::before {
      content: '';
      position: absolute;
      left: -12px;
      top: 5px;
      width: 6px;
      height: 6px;
      border-radius: 50%;
      background: var(--accent);
      box-shadow: 0 0 6px var(--accent);
      border: 1.5px solid var(--bg);
    }

    .plan-action-step.blocked::before {
      background: var(--bad);
      box-shadow: 0 0 6px var(--bad);
    }

    .filters-card {
      padding: 12px;
      border-radius: 8px;
      margin-bottom: 16px;
      display: flex;
      flex-direction: column;
      gap: 8px;
    }

    .filter-row {
      display: flex;
      align-items: center;
      gap: 8px;
      font-size: 12px;
      cursor: pointer;
      user-select: none;
    }

    .filter-row input {
      cursor: pointer;
    }

    .node.server { --node-color: var(--server); }
    .node.agent { --node-color: var(--agent); }
    .node.dweller { --node-color: var(--dweller); }
    .node.network { --node-color: var(--network); }
    .node.port_forward { --node-color: var(--forward); }

    .minimap-ring {
      fill: none;
      stroke: rgba(255, 255, 255, 0.05);
      stroke-width: 0.8;
    }

    .edge-label-pill {
      fill: rgba(10, 15, 26, 0.9);
      stroke: var(--border);
      stroke-width: 1;
    }

    .edge-label {
      fill: var(--muted);
      font-size: 10px;
      font-weight: 600;
      pointer-events: none;
      font-family: 'Inter', sans-serif;
    }

    @media (prefers-reduced-motion: reduce) {
      .edge.encrypted, .edge.unencrypted, .edge.highlight-path {
        animation: none;
      }
    }

    @media (max-width: 1024px) {
      main {
        grid-template-columns: 1fr;
        grid-template-rows: 1fr 1fr;
        height: auto;
        overflow: visible;
      }
      .shell {
        height: auto;
        overflow: auto;
      }
      aside {
        border-left: none;
        border-top: 1px solid var(--border);
        height: 600px;
      }
      #map-wrap, #map {
        height: 45vh;
        min-height: 380px;
      }
    }
  </style>
</head>
<body>
  <div class="shell">
    <header>
      <div class="brand">
        <div class="brand-mark" aria-hidden="true">
          <svg viewBox="0 0 24 24" width="20" height="20">
            <path d="M4 8h5v5H4zM15 4h5v5h-5zM15 15h5v5h-5z" fill="none" stroke="#60a5fa" stroke-width="1.8" stroke-linejoin="round"/>
            <path d="M9 10.5h3.5V6.5H15M9 10.5h3.5V17.5H15" fill="none" stroke="#34d399" stroke-width="1.8" stroke-linecap="round"/>
          </svg>
        </div>
        <div>
          <h1>Labyrinth</h1>
          <span>Smart Access & Route Visualization Dashboard</span>
        </div>
      </div>
      <div class="header-meta">
        <span class="status-chip">
          <i id="status-dot" class="status-dot"></i>
          <span id="status-text">Connecting</span>
        </span>
        <span id="updated" style="color: var(--muted)">Waiting for snapshot</span>
      </div>
    </header>
    <main>
      <section id="map-wrap" aria-label="Network visualization">
        <div class="map-toolbar">
          <div class="map-search-bar">
            <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="var(--muted)" stroke-width="2"><circle cx="11" cy="11" r="8"/><path d="M21 21l-4.3-4.3"/></svg>
            <input type="text" id="node-search" placeholder="Search nodes, subnets..." oninput="onSearchChange(this.value)">
          </div>

          <div class="map-controls-row">
            <button class="tool btn-tool" id="zoom-in" title="Zoom in" aria-label="Zoom in">+</button>
            <button class="tool btn-tool" id="zoom-out" title="Zoom out" aria-label="Zoom out">-</button>
            <button class="tool btn-tool" id="zoom-fit" title="Fit map" aria-label="Fit map">⌂</button>
            <button class="tool btn-tool" id="reset-pins" title="Unpin all nodes" aria-label="Unpin all nodes">⟳</button>
            <button class="tool btn-tool" id="pause-poll" title="Pause polling" aria-label="Pause polling">⏸</button>
          </div>
        </div>

        <svg id="map" role="img" aria-label="Labyrinth network topology">
          <g id="world"></g>
        </svg>

        <div id="empty" class="empty-state" style="position: absolute; inset: 0; display: none; align-items: center; justify-content: center; pointer-events: none;">
          <div class="glass" style="padding: 20px; border-radius: 8px; max-width: 380px; pointer-events: auto;">
            <h3 style="margin: 0 0 6px; font-family: 'Outfit'; font-size: 15px;">No Connected Agents</h3>
            <p style="margin: 0; font-size: 12px; color: var(--muted);">Start an agent or register a dweller listener to populate the interactive topology.</p>
          </div>
        </div>
      </section>

      <aside>
        <div class="sidebar-tabs">
          <button class="tab-btn active" data-tab="tab-overview" onclick="showTab('tab-overview')">
            <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12l9-9 9 9M5 10v10a1 1 0 001 1h3a1 1 0 001-1v-4a1 1 0 011-1h2a1 1 0 011 1v4a1 1 0 001 1h3a1 1 0 001-1V10"/></svg>
            Overview
          </button>
          <button class="tab-btn" data-tab="tab-inventory" onclick="showTab('tab-inventory')">
            <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M4 6h16M4 12h16M4 18h16"/></svg>
            Inventory
          </button>
          <button class="tab-btn" data-tab="tab-planner" onclick="showTab('tab-planner')">
            <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 5H7a2 2 0 00-2 2v12a2 2 0 002 2h10a2 2 0 002-2V7a2 2 0 00-2-2h-2M9 5a2 2 0 002 2h2a2 2 0 002-2M9 5a2 2 0 012-2h2a2 2 0 012 2m-6 9l2 2 4-4"/></svg>
            Access Planner
          </button>
        </div>

        <div class="sidebar-content">
          <!-- Overview Pane -->
          <div id="tab-overview" class="tab-pane active">
            <div class="metrics-grid" id="metrics-container"></div>

            <div id="selected-inspect-container"></div>

            <div class="section-title">Active Tunnels</div>
            <div class="list-container" id="overview-tunnels"></div>

            <div class="section-title">Portal Forwards</div>
            <div class="list-container" id="overview-forwards"></div>

            <div class="section-title">Legend</div>
            <div class="legend-card glass">
              <div class="legend-grid">
                <div class="legend-item"><span class="legend-color" style="background:var(--server); --legend-c:var(--server)"></span>Server / gateway</div>
                <div class="legend-item"><span class="legend-color" style="background:var(--agent); --legend-c:var(--agent)"></span>Agent</div>
                <div class="legend-item"><span class="legend-color" style="background:var(--dweller); --legend-c:var(--dweller)"></span>Dweller</div>
                <div class="legend-item"><span class="legend-color" style="background:var(--network); --legend-c:var(--network)"></span>Detected Network</div>
                <div class="legend-item"><span class="legend-color" style="background:var(--forward); --legend-c:var(--forward)"></span>Portal Port Forward</div>
                <div class="legend-item"><span class="legend-line" style="background:var(--ok); box-shadow:0 0 4px var(--ok)"></span>Encrypted link</div>
                <div class="legend-item"><span class="legend-line" style="background:var(--warn); box-shadow:0 0 4px var(--warn)"></span>Unencrypted/local link</div>
              </div>
            </div>
          </div>

          <!-- Inventory Pane -->
          <div id="tab-inventory" class="tab-pane">
            <div class="filters-card glass">
              <div class="filter-row">
                <input type="checkbox" id="chk-hide-offline" onchange="onFilterChange('hide-offline', this.checked)">
                <label for="chk-hide-offline">Hide offline dwellers</label>
              </div>
              <div class="filter-row">
                <input type="checkbox" id="chk-hide-networks" onchange="onFilterChange('hide-networks', this.checked)">
                <label for="chk-hide-networks">Hide detected subnets</label>
              </div>
              <div class="filter-row">
                <input type="checkbox" id="chk-hide-forwards" onchange="onFilterChange('hide-forwards', this.checked)">
                <label for="chk-hide-forwards">Hide port forwards</label>
              </div>
            </div>

            <div class="section-title">Connected Nodes</div>
            <div class="list-container" id="inventory-nodes"></div>

            <div class="section-title">Detected Subnets</div>
            <div class="list-container" id="inventory-networks"></div>

            <div class="section-title">Shared Subnets</div>
            <div class="list-container" id="inventory-shared"></div>

            <div class="section-title">Route Conflicts</div>
            <div class="list-container" id="inventory-conflicts"></div>
          </div>

          <!-- Planner Pane -->
          <div id="tab-planner" class="tab-pane">
            <div style="font-size:12.5px; color:var(--muted); margin-bottom:12px; line-height:1.4;">
              Click an access plan to inspect actions and highlight the multi-hop paths visually in the network topology map.
            </div>
            <div class="list-container" id="planner-chains"></div>
          </div>
        </div>
      </aside>
    </main>
  </div>

  <script>
    const svg = document.getElementById('map');
    const world = document.getElementById('world');
    const empty = document.getElementById('empty');

    const colors = {
      server: '#3b82f6',
      agent: '#10b981',
      dweller: '#a855f7',
      network: '#f59e0b',
      port_forward: '#f97316'
    };

    const icons = {
      server: [
        ['rect', { x: -11, y: -11, width: 22, height: 22, rx: 3 }],
        ['line', { x1: -7, y1: -4, x2: 7, y2: -4 }],
        ['line', { x1: -7, y1: 4, x2: 7, y2: 4 }],
        ['circle', { cx: 4, cy: -4, r: 1, fill: 'currentColor' }],
        ['circle', { cx: 4, cy: 4, r: 1, fill: 'currentColor' }]
      ],
      agent: [
        ['rect', { x: -12, y: -9, width: 24, height: 16, rx: 2 }],
        ['line', { x1: -6, y1: 11, x2: 6, y2: 11 }],
        ['line', { x1: -2, y1: 7, x2: -2, y2: 11 }],
        ['line', { x1: 2, y1: 7, x2: 2, y2: 11 }]
      ],
      dweller: [
        ['path', { d: 'M-12 -2 L0 -11 L12 -2 L12 11 L-12 11 Z' }],
        ['rect', { x: -3, y: 3, width: 6, height: 8 }]
      ],
      network: [
        ['circle', { cx: 0, cy: 0, r: 11 }],
        ['line', { x1: -11, y1: 0, x2: 11, y2: 0 }],
        ['line', { x1: 0, y1: -11, x2: 0, y2: 11 }]
      ],
      port_forward: [
        ['path', { d: 'M-10 -7 L-1 -7 L-1 7 L-10 7 Z M1 -7 L10 -7 L10 7 L1 7 Z' }],
        ['path', { d: 'M-4 0 L4 0 M2 -3 L5 0 L2 3' }]
      ]
    };

    // Globals
    let current = { nodes: [], edges: [], routes: [], port_forwards: [], ariadne: [], shared_networks: [], conflicts: [], chain_plans: [] };
    let nodes = [];
    let edges = [];
    let nodeMap = new Map();
    let selectedNodeId = '';
    let activeHighlightPath = null;
    let pollPaused = false;

    let viewport = { x: 0, y: 0, scale: 1 };
    let pan = { active: false, x: 0, y: 0, moved: false };
    let draggedNode = null;
    let width = 800;
    let height = 600;
    let simulationActive = false;

    // Filter parameters
    let searchText = '';
    let filterHideOfflineDweller = false;
    let filterHideNetworks = false;
    let filterHidePortForwards = false;

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

    function updateDimensions() {
      const bounds = svg.getBoundingClientRect();
      width = Math.max(bounds.width, 700);
      height = Math.max(bounds.height, 440);
      svg.setAttribute('viewBox', `0 0 ${width} ${height}`);
    }

    function nodeRadius(node) {
      if (node.kind === 'server') return 28;
      if (node.kind === 'network') return 20;
      if (node.kind === 'port_forward') return 20;
      return 23;
    }

    function edgeTouches(edge, nodeId) {
      return edge.source === nodeId || edge.target === nodeId;
    }

    function isNodeFiltered(node) {
      if (!node) return false;
      if (searchText) {
        const query = searchText.toLowerCase();
        const matchesLabel = node.label.toLowerCase().includes(query);
        const matchesDetail = (node.detail || '').toLowerCase().includes(query);
        const matchesStatus = (node.status || '').toLowerCase().includes(query);
        if (!matchesLabel && !matchesDetail && !matchesStatus) return true;
      }

      if (filterHideOfflineDweller && node.kind === 'dweller' && node.status.toLowerCase().includes('offline')) {
        return true;
      }
      if (filterHideNetworks && node.kind === 'network') {
        return true;
      }
      if (filterHidePortForwards && node.kind === 'port_forward') {
        return true;
      }

      return false;
    }

    function initPositions(dataNodes, dataEdges) {
      const oldNodeMap = new Map(nodes.map(n => [n.id, n]));
      const center = { x: width * 0.44, y: height * 0.52 };

      nodes = dataNodes.map(dn => {
        const existing = oldNodeMap.get(dn.id);
        if (existing) {
          return {
            ...dn,
            x: existing.x,
            y: existing.y,
            vx: existing.vx,
            vy: existing.vy,
            pinned: existing.pinned
          };
        } else {
          const angle = Math.random() * Math.PI * 2;
          const radius = dn.kind === 'server' ? 0 : dn.kind === 'agent' || dn.kind === 'dweller' ? 130 : 230;
          return {
            ...dn,
            x: center.x + Math.cos(angle) * radius + (Math.random() - 0.5) * 15,
            y: center.y + Math.sin(angle) * radius + (Math.random() - 0.5) * 15,
            vx: 0,
            vy: 0,
            pinned: dn.kind === 'server'
          };
        }
      });

      edges = dataEdges.map(de => ({ ...de }));
      nodeMap = new Map(nodes.map(n => [n.id, n]));
    }

    function setupDefs() {
      let defs = svg.querySelector('defs');
      if (!defs) {
        defs = el('defs', {}, svg);
      } else {
        defs.innerHTML = '';
      }

      // Gradients
      const grServer = el('linearGradient', { id: 'grad-server', x1: '0%', y1: '0%', x2: '100%', y2: '100%' }, defs);
      el('stop', { offset: '0%', 'stop-color': '#60a5fa' }, grServer);
      el('stop', { offset: '100%', 'stop-color': '#1d4ed8' }, grServer);

      const grAgent = el('linearGradient', { id: 'grad-agent', x1: '0%', y1: '0%', x2: '100%', y2: '100%' }, defs);
      el('stop', { offset: '0%', 'stop-color': '#34d399' }, grAgent);
      el('stop', { offset: '100%', 'stop-color': '#047857' }, grAgent);

      const grDweller = el('linearGradient', { id: 'grad-dweller', x1: '0%', y1: '0%', x2: '100%', y2: '100%' }, defs);
      el('stop', { offset: '0%', 'stop-color': '#c084fc' }, grDweller);
      el('stop', { offset: '100%', 'stop-color': '#6b21a8' }, grDweller);

      const grNetwork = el('linearGradient', { id: 'grad-network', x1: '0%', y1: '0%', x2: '100%', y2: '100%' }, defs);
      el('stop', { offset: '0%', 'stop-color': '#fbbf24' }, grNetwork);
      el('stop', { offset: '100%', 'stop-color': '#b45309' }, grNetwork);

      const grPF = el('linearGradient', { id: 'grad-port_forward', x1: '0%', y1: '0%', x2: '100%', y2: '100%' }, defs);
      el('stop', { offset: '0%', 'stop-color': '#fb923c' }, grPF);
      el('stop', { offset: '100%', 'stop-color': '#c2410c' }, grPF);

      // Glow filters
      const glow = el('filter', { id: 'soft-glow', x: '-40%', y: '-40%', width: '180%', height: '180%' }, defs);
      el('feGaussianBlur', { stdDeviation: 5, result: 'blur' }, glow);
      const merge = el('feMerge', {}, glow);
      el('feMergeNode', { in: 'blur' }, merge);
      el('feMergeNode', { in: 'SourceGraphic' }, merge);
    }

    function syncSvgDom() {
      // Re-setup background layout rings if not exist
      let bgLayer = document.getElementById('background-layer');
      if (!bgLayer) {
        bgLayer = el('g', { id: 'background-layer' }, world);
        // Put it at the back
        world.insertBefore(bgLayer, world.firstChild);
      }

      const center = nodeMap.get('server');
      if (center) {
        bgLayer.innerHTML = '';
        [130, 230, 330].forEach(radius => el('circle', {
          cx: center.x,
          cy: center.y,
          r: radius,
          class: 'minimap-ring'
        }, bgLayer));
      }

      // 1. Edges
      const edgeGroup = document.getElementById('edges-group') || el('g', { id: 'edges-group' }, world);
      const currentEdgeIds = new Set(edges.map(e => `edge-${e.source}-${e.target}-${e.kind}`));
      Array.from(edgeGroup.children).forEach(child => {
        if (!currentEdgeIds.has(child.id)) {
          child.remove();
        }
      });

      for (const edge of edges) {
        const edgeId = `edge-${edge.source}-${edge.target}-${edge.kind}`;
        let edgeEl = document.getElementById(edgeId);
        const isSelected = selectedNodeId && edgeTouches(edge, selectedNodeId);
        const faded = selectedNodeId && !isSelected && selectedNodeId !== '';
        const inHighlightPath = activeHighlightPath && activeHighlightPath.includes(edge.source) && activeHighlightPath.includes(edge.target);

        let cls = `edge ${edge.encrypted ? 'encrypted' : 'unencrypted'} ${edge.kind.replace('_', '-')}`;
        if (isSelected) cls += ' selected';
        if (inHighlightPath) cls += ' highlight-path';

        const isFiltered = isNodeFiltered(nodeMap.get(edge.source)) || isNodeFiltered(nodeMap.get(edge.target));
        if (isFiltered || (faded && !inHighlightPath)) cls += ' filtered-out';

        if (!edgeEl) {
          edgeEl = el('line', { id: edgeId, class: cls }, edgeGroup);
        } else {
          edgeEl.setAttribute('class', cls);
        }
      }

      // 2. Edge labels
      const labelsGroup = document.getElementById('labels-group') || el('g', { id: 'labels-group' }, world);
      const currentLabelIds = new Set(edges.map(e => `label-${e.source}-${e.target}-${e.kind}`));
      Array.from(labelsGroup.children).forEach(child => {
        if (!currentLabelIds.has(child.id)) {
          child.remove();
        }
      });

      for (const edge of edges) {
        const labelId = `label-${edge.source}-${edge.target}-${edge.kind}`;
        let labelGroupEl = document.getElementById(labelId);
        const isFiltered = isNodeFiltered(nodeMap.get(edge.source)) || isNodeFiltered(nodeMap.get(edge.target));
        const faded = selectedNodeId && selectedNodeId !== '' && !edgeTouches(edge, selectedNodeId) && !(activeHighlightPath && activeHighlightPath.includes(edge.source) && activeHighlightPath.includes(edge.target));

        let cls = 'edge-label-container';
        if (isFiltered || faded) cls += ' filtered-out';

        if (!labelGroupEl) {
          labelGroupEl = el('g', { id: labelId, class: cls }, labelsGroup);
          el('rect', { class: 'edge-label-pill' }, labelGroupEl);
          const textEl = el('text', { class: 'edge-label', 'text-anchor': 'middle' }, labelGroupEl);
          textEl.textContent = edge.label;
        } else {
          labelGroupEl.setAttribute('class', cls);
        }
      }

      // 3. Nodes
      const nodeGroup = document.getElementById('nodes-group') || el('g', { id: 'nodes-group' }, world);
      const currentNodeIds = new Set(nodes.map(n => n.id));
      Array.from(nodeGroup.children).forEach(child => {
        const nodeId = child.getAttribute('data-node-id');
        if (!currentNodeIds.has(nodeId)) {
          child.remove();
        }
      });

      for (const node of nodes) {
        const nodeId = node.id;
        let nodeEl = nodeGroup.querySelector(`[data-node-id="${nodeId}"]`);
        const radius = nodeRadius(node);
        const color = colors[node.kind] || '#99a6b3';
        const connected = selectedNodeId && edges.some(edge => edgeTouches(edge, selectedNodeId) && edgeTouches(edge, node.id));
        const selected = node.id === selectedNodeId;
        const inHighlightPath = activeHighlightPath && activeHighlightPath.includes(node.id);
        const faded = selectedNodeId && !selected && !connected && selectedNodeId !== '' && !inHighlightPath;
        const isFiltered = isNodeFiltered(node);

        let cls = `node ${node.kind}`;
        if (selected) cls += ' selected';
        if (node.pinned) cls += ' pinned';
        if (inHighlightPath) cls += ' highlighted';
        if (faded || isFiltered) cls += ' filtered-out';

        if (!nodeEl) {
          nodeEl = el('g', {
            'data-node-id': nodeId,
            class: cls,
            tabindex: '0',
            role: 'button',
            'aria-label': `${node.label} ${node.status}`
          }, nodeGroup);

          nodeEl.addEventListener('click', (e) => {
            if (pan.moved) return;
            e.stopPropagation();
            selectNode(nodeId);
          });
          nodeEl.addEventListener('dblclick', (e) => {
            e.stopPropagation();
            if (node.kind !== 'server') {
              node.pinned = false;
              nodeEl.classList.remove('pinned');
              startSimulation();
            }
          });
          nodeEl.addEventListener('keydown', (e) => {
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault();
              selectNode(nodeId);
            }
          });

          el('circle', { r: radius + 11, fill: color, class: 'node-halo', filter: 'url(#soft-glow)' }, nodeEl);
          el('circle', { r: radius, fill: `url(#grad-${node.kind})`, class: 'node-frame' }, nodeEl);

          const iconDefs = icons[node.kind] || icons.agent;
          for (const [name, attrs] of iconDefs) {
            el(name, { ...attrs, class: 'node-icon' }, nodeEl);
          }

          el('text', { x: 0, y: radius + 18, 'text-anchor': 'middle' }, nodeEl).textContent = shortLabel(node.label, 20);
          el('text', { x: 0, y: radius + 30, 'text-anchor': 'middle', class: 'subtext' }, nodeEl).textContent = shortLabel(node.status, 24);

          const titleEl = el('title', {}, nodeEl);
          titleEl.textContent = `${node.label}\n${node.status}\n${node.detail || ''}`;
        } else {
          nodeEl.setAttribute('class', cls);
          const texts = nodeEl.querySelectorAll('text');
          if (texts[0]) texts[0].textContent = shortLabel(node.label, 20);
          if (texts[1]) texts[1].textContent = shortLabel(node.status, 24);
          const titleEl = nodeEl.querySelector('title');
          if (titleEl) titleEl.textContent = `${node.label}\n${node.status}\n${node.detail || ''}`;
        }
      }
    }

    function updateSvgElements() {
      // 1. Update lines
      for (const edge of edges) {
        const edgeId = `edge-${edge.source}-${edge.target}-${edge.kind}`;
        const edgeEl = document.getElementById(edgeId);
        const a = nodeMap.get(edge.source);
        const b = nodeMap.get(edge.target);
        if (edgeEl && a && b) {
          edgeEl.setAttribute('x1', a.x);
          edgeEl.setAttribute('y1', a.y);
          edgeEl.setAttribute('x2', b.x);
          edgeEl.setAttribute('y2', b.y);
        }
      }

      // 2. Update edge labels
      for (const edge of edges) {
        const labelId = `label-${edge.source}-${edge.target}-${edge.kind}`;
        const labelGroupEl = document.getElementById(labelId);
        const a = nodeMap.get(edge.source);
        const b = nodeMap.get(edge.target);
        if (labelGroupEl && a && b) {
          const labelT = edge.kind === 'route' ? 0.68 : 0.5;
          const midX = a.x + (b.x - a.x) * labelT;
          const midY = a.y + (b.y - a.y) * labelT;

          const rectEl = labelGroupEl.querySelector('rect');
          const textEl = labelGroupEl.querySelector('text');

          const labelWidth = Math.min(Math.max(edge.label.length * 6.5 + 14, 52), 130);

          if (rectEl) {
            rectEl.setAttribute('x', midX - labelWidth / 2);
            rectEl.setAttribute('y', midY - 9);
            rectEl.setAttribute('width', labelWidth);
            rectEl.setAttribute('height', 16);
            rectEl.setAttribute('rx', 4);
          }
          if (textEl) {
            textEl.setAttribute('x', midX);
            textEl.setAttribute('y', midY + 3);
          }
        }
      }

      // 3. Update nodes
      for (const node of nodes) {
        const nodeEl = document.querySelector(`[data-node-id="${node.id}"]`);
        if (nodeEl) {
          nodeEl.setAttribute('transform', `translate(${node.x},${node.y})`);
        }
      }
    }

    function stepSimulation() {
      if (!simulationActive) return;

      const center = { x: width * 0.44, y: height * 0.52 };
      const damping = 0.82;
      let maxVelocity = 0;

      // Repulsion
      for (let i = 0; i < nodes.length; i++) {
        const a = nodes[i];
        if (isNodeFiltered(a)) continue;
        for (let j = i + 1; j < nodes.length; j++) {
          const b = nodes[j];
          if (isNodeFiltered(b)) continue;

          let dx = a.x - b.x;
          let dy = a.y - b.y;
          let distSq = dx * dx + dy * dy;
          if (distSq < 1) distSq = 1;
          let dist = Math.sqrt(distSq);

          const force = 3400 / distSq;
          dx /= dist;
          dy /= dist;

          if (!a.pinned) { a.vx += dx * force; a.vy += dy * force; }
          if (!b.pinned) { b.vx -= dx * force; b.vy -= dy * force; }
        }
      }

      // Attraction
      for (const edge of edges) {
        const a = nodeMap.get(edge.source);
        const b = nodeMap.get(edge.target);
        if (!a || !b) continue;
        if (isNodeFiltered(a) || isNodeFiltered(b)) continue;

        let dx = b.x - a.x;
        let dy = b.y - a.y;
        let dist = Math.sqrt(dx * dx + dy * dy);
        if (dist < 1) dist = 1;

        let desired = 150;
        if (edge.kind === 'route') desired = 130;
        else if (edge.kind === 'portal') desired = 110;

        const force = (dist - desired) * 0.038;
        dx /= dist;
        dy /= dist;

        if (!a.pinned) { a.vx += dx * force; a.vy += dy * force; }
        if (!b.pinned) { b.vx -= dx * force; b.vy -= dy * force; }
      }

      // Central Pull & Integration
      for (const node of nodes) {
        if (node.kind === 'server') {
          node.x = center.x;
          node.y = center.y;
          node.vx = 0;
          node.vy = 0;
          continue;
        }

        if (node.pinned) {
          node.vx = 0;
          node.vy = 0;
          continue;
        }

        // Gravity
        node.vx += (center.x - node.x) * 0.0035;
        node.vy += (center.y - node.y) * 0.0035;

        node.x += node.vx;
        node.y += node.vy;

        node.vx *= damping;
        node.vy *= damping;

        // Boundaries
        node.x = Math.max(40, Math.min(width - 40, node.x));
        node.y = Math.max(40, Math.min(height - 40, node.y));

        maxVelocity = Math.max(maxVelocity, Math.abs(node.vx) + Math.abs(node.vy));
      }

      updateSvgElements();

      if (maxVelocity < 0.05) {
        simulationActive = false;
      } else {
        requestAnimationFrame(stepSimulation);
      }
    }

    function startSimulation() {
      if (!simulationActive) {
        simulationActive = true;
        requestAnimationFrame(stepSimulation);
      }
    }

    function applyViewport() {
      world.setAttribute('transform', `translate(${viewport.x},${viewport.y}) scale(${viewport.scale})`);
    }

    function zoomAt(clientX, clientY, factor) {
      const rect = svg.getBoundingClientRect();
      const x = clientX - rect.left;
      const y = clientY - rect.top;
      const next = Math.max(0.25, Math.min(4.0, viewport.scale * factor));
      const ratio = next / viewport.scale;
      viewport.x = x - (x - viewport.x) * ratio;
      viewport.y = y - (y - viewport.y) * ratio;
      viewport.scale = next;
      applyViewport();
    }

    function fitMap() {
      viewport.x = 0;
      viewport.y = 0;
      viewport.scale = 1;
      applyViewport();
    }

    function resetPins() {
      for (const node of nodes) {
        if (node.kind !== 'server') {
          node.pinned = false;
          const nodeEl = document.querySelector(`[data-node-id="${node.id}"]`);
          if (nodeEl) nodeEl.classList.remove('pinned');
        }
      }
      startSimulation();
    }

    function shortLabel(value, max) {
      const text = String(value || '');
      return text.length > max ? `${text.slice(0, max - 1)}…` : text;
    }

    function showTab(tabId) {
      document.querySelectorAll('.tab-btn').forEach(btn => {
        btn.classList.toggle('active', btn.getAttribute('data-tab') === tabId);
      });
      document.querySelectorAll('.tab-pane').forEach(pane => {
        pane.classList.toggle('active', pane.id === tabId);
      });
    }

    function selectNode(nodeId) {
      selectedNodeId = nodeId || '';
      syncSvgDom();
      renderSide(current);
    }

    function togglePlanHighlight(planIdx) {
      const plan = current.chain_plans[planIdx];
      if (!plan) return;

      const targetNodeId = `network:${plan.target}`;
      const isAlreadyHighlighted = activeHighlightPath && activeHighlightPath[activeHighlightPath.length - 1] === targetNodeId;

      if (isAlreadyHighlighted) {
        activeHighlightPath = null;
      } else {
        const path = ['server'];
        for (const action of plan.actions) {
          if (action.StartTunnel) {
            path.push(`agent:${action.StartTunnel.agent_id}`);
            path.push(`network:${action.StartTunnel.cidr}`);
          } else if (action.ReuseTunnel) {
            path.push(`agent:${action.ReuseTunnel.agent_id}`);
            path.push(`network:${action.ReuseTunnel.cidr}`);
          } else if (action.ConnectDweller) {
            path.push(`agent:${action.ConnectDweller.dweller_id}`);
          } else if (action.RetryAfterDweller) {
            path.push(`agent:${action.RetryAfterDweller.dweller_id}`);
          }
        }
        if (!path.includes(targetNodeId)) {
          path.push(targetNodeId);
        }
        activeHighlightPath = path;
      }

      syncSvgDom();
      renderSide(current);
    }

    function onSearchChange(val) {
      searchText = val;
      syncSvgDom();
      renderSide(current);
    }

    function onFilterChange(filterId, checked) {
      if (filterId === 'hide-offline') filterHideOfflineDweller = checked;
      if (filterId === 'hide-networks') filterHideNetworks = checked;
      if (filterId === 'hide-forwards') filterHidePortForwards = checked;

      syncSvgDom();
      renderSide(current);
      startSimulation();
    }

    function item(title, meta, pills = [], nodeId = '') {
      const isSelected = nodeId && nodeId === selectedNodeId;
      const isSelectedPlan = activeHighlightPath && activeHighlightPath[activeHighlightPath.length - 1] === nodeId;
      const nodeAttr = nodeId ? ` onclick="selectNode('${escapeHtml(nodeId)}'); event.stopPropagation();"` : '';

      let pillsHtml = pills.map(p => {
        let toneClass = '';
        if (p.tone === 'ok') toneClass = 'success';
        else if (p.tone === 'warn') toneClass = 'warning';
        else if (p.tone === 'bad') toneClass = 'danger';
        return `<span class="pill-badge ${toneClass}">${escapeHtml(p.text)}</span>`;
      }).join('');

      return `
        <div class="list-item ${isSelected ? 'selected' : ''} ${isSelectedPlan ? 'selected' : ''}" ${nodeAttr}>
          <div class="item-header">
            <span>${escapeHtml(title)}</span>
          </div>
          ${meta ? `<div class="item-desc">${escapeHtml(meta)}</div>` : ''}
          ${pills.length ? `<div class="item-pills">${pillsHtml}</div>` : ''}
        </div>
      `;
    }

    function emptyText(text) {
      return `<div class="empty-state">${escapeHtml(text)}</div>`;
    }

    function selectedDetails(data) {
      const selected = nodes.find(node => node.id === selectedNodeId);
      if (!selectedNodeId || !selected) {
        return `
          <div class="empty-state" style="border-style: solid;">
            <svg viewBox="0 0 24 24" width="20" height="20" style="margin-bottom: 6px; opacity: 0.5;" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>
            Click a node on the topology map or inventory list to inspect detailed parameters.
          </div>
        `;
      }

      const connectedEdges = edges.filter(edge => edgeTouches(edge, selected.id));
      const badgeColor = colors[selected.kind] || '#99a6b3';

      let kindText = selected.kind.replace('_', ' ');
      let statusClass = selected.status.toLowerCase().includes('offline') ? 'danger' : 'success';
      if (selected.status.toLowerCase().includes('detected') || selected.status.toLowerCase().includes('active')) statusClass = 'success';
      if (selected.status.toLowerCase().includes('remembered')) statusClass = 'warning';

      let properties = `
        <tr><td>ID</td><td>${escapeHtml(selected.id)}</td></tr>
        <tr><td>Name</td><td>${escapeHtml(selected.label)}</td></tr>
        <tr><td>Kind</td><td><span class="inspect-badge" style="background: ${badgeColor}20; color: ${badgeColor}; border: 1px solid ${badgeColor}40;">${kindText}</span></td></tr>
        <tr><td>Status</td><td><span class="pill-badge ${statusClass}">${escapeHtml(selected.status)}</span></td></tr>
      `;

      if (selected.detail) {
        properties += `<tr><td>Detail</td><td>${escapeHtml(selected.detail)}</td></tr>`;
      }

      let neighborsHtml = '';
      if (connectedEdges.length > 0) {
        neighborsHtml = `<div class="section-title">Connected Links (${connectedEdges.length})</div><div class="list-container">`;
        for (const edge of connectedEdges) {
          const otherId = edge.source === selected.id ? edge.target : edge.source;
          const otherNode = nodeMap.get(otherId);
          if (otherNode) {
            neighborsHtml += `
              <div class="list-item" onclick="selectNode('${otherNode.id}'); event.stopPropagation();" style="cursor: pointer;">
                <div class="item-header">
                  <span>${escapeHtml(otherNode.label)}</span>
                  <span class="pill-badge" style="background: ${colors[otherNode.kind]}15; color: ${colors[otherNode.kind]}">${otherNode.kind}</span>
                </div>
                <div class="item-desc">Label: ${escapeHtml(edge.label)} (${edge.encrypted ? 'Encrypted' : 'Unencrypted'})</div>
              </div>
            `;
          }
        }
        neighborsHtml += '</div>';
      }

      return `
        <div class="inspect-card glass" style="--card-color: ${badgeColor}">
          <div class="inspect-title">
            <span>Node Inspector</span>
            <button class="btn-tool" onclick="selectNode(''); event.stopPropagation();" style="border:none; background:transparent; font-size:16px; cursor:pointer; color:var(--muted);">&times;</button>
          </div>
          <table class="inspect-prop-table">
            <tbody>
              ${properties}
            </tbody>
          </table>
          ${neighborsHtml}
        </div>
      `;
    }

    function chainSummary(plan) {
      const actions = plan.actions || [];
      const blocked = actions.find(action => action.Blocked || action.blocked);
      if (blocked) return blocked.Blocked?.reason || blocked.blocked?.reason || 'Blocked';
      return actions.map(action => {
        if (action.StartTunnel) return `start ${action.StartTunnel.cidr}`;
        if (action.ReuseTunnel) return `reuse ${action.ReuseTunnel.cidr}`;
        if (action.ConnectDweller) return `connect ${action.ConnectDweller.dweller_name}`;
        if (action.RetryAfterDweller) return 'refresh';
        return 'step';
      }).join(' → ');
    }

    function renderPlanActionsDetail(plan) {
      return `
        <div class="plan-actions-list" style="margin-top: 10px; border-top: 1px solid var(--border); padding-top: 8px;">
          ${plan.actions.map(action => {
            let text = '';
            let isBlocked = false;
            if (action.ReuseTunnel) {
              text = `Reuse active tunnel ${action.ReuseTunnel.cidr} via ${action.ReuseTunnel.agent_name}`;
            } else if (action.StartTunnel) {
              text = `Start tunnel ${action.StartTunnel.cidr} via ${action.StartTunnel.agent_name}`;
            } else if (action.ConnectDweller) {
              text = `Connect remembered dweller ${action.ConnectDweller.dweller_name} at ${action.ConnectDweller.address}`;
            } else if (action.RetryAfterDweller) {
              text = `Refresh topology after ${action.RetryAfterDweller.dweller_name} registers`;
            } else if (action.Blocked) {
              text = `Blocked: ${action.Blocked.reason}`;
              isBlocked = true;
            }
            return `
              <div class="plan-action-step ${isBlocked ? 'blocked' : ''}">
                <div style="font-size:11.5px; font-weight:600; color: ${isBlocked ? 'var(--bad)' : 'var(--text)'}">${escapeHtml(text)}</div>
              </div>
            `;
          }).join('')}
        </div>
      `;
    }

    function renderSide(data) {
      // 1. Overview Tab
      const s = data.summary;
      document.getElementById('metrics-container').innerHTML = `
        <div class="metric-card glass" style="--card-color: var(--agent)">
          <h3>Agents Online</h3>
          <div class="val">${s.agents_online}</div>
        </div>
        <div class="metric-card glass" style="--card-color: var(--dweller)">
          <h3>Dwellers</h3>
          <div class="val">${s.dwellers_online} <span style="font-size:11px; color:var(--muted)">/ ${s.dwellers_total}</span></div>
        </div>
        <div class="metric-card glass" style="--card-color: var(--network)">
          <h3>Subnets</h3>
          <div class="val">${s.detected_networks}</div>
        </div>
        <div class="metric-card glass" style="--card-color: var(--forward)">
          <h3>Tunnels</h3>
          <div class="val">${s.active_tunnels}</div>
        </div>
      `;

      document.getElementById('selected-inspect-container').innerHTML = selectedDetails(data);

      document.getElementById('overview-tunnels').innerHTML = data.ariadne.length
        ? data.ariadne.map(t => item(t.agent_name, `Proxy port: ${t.proxy_port}`, [{ text: 'tun/tls/enc', tone: 'ok' }], `agent:${t.agent_id}`)).join('')
        : emptyText('No active Ariadne tunnels.');

      document.getElementById('overview-forwards').innerHTML = data.port_forwards.length
        ? data.port_forwards.map(f => item(`localhost:${f.local_port}`, `${f.agent_name} → ${f.target_host}:${f.target_port}`, [{ text: 'local/unenc', tone: 'warn' }, { text: 'stream/tls/enc', tone: 'ok' }], `port-forward:${f.local_port}`)).join('')
        : emptyText('No active Portal forwards.');

      // 2. Inventory Tab
      const filteredNodes = data.nodes.filter(node => (node.kind === 'agent' || node.kind === 'dweller') && !isNodeFiltered(node));
      document.getElementById('inventory-nodes').innerHTML = filteredNodes.length
        ? filteredNodes.map(node => item(node.label, node.detail, [{ text: node.kind }, { text: node.status, tone: node.status.includes('offline') ? 'warn' : 'ok' }], node.id)).join('')
        : emptyText('No matching agents or dwellers.');

      const filteredRoutes = data.routes.filter(r => !isNodeFiltered({ kind: 'network', label: r.cidr, status: 'detected' }));
      document.getElementById('inventory-networks').innerHTML = filteredRoutes.length
        ? filteredRoutes.map(route => item(route.cidr, `${route.agent_name} via ${route.interface_name} (${route.source_address})`, [{ text: `score ${route.score}` }, { text: 'detected', tone: 'ok' }], `network:${route.cidr}`)).join('')
        : emptyText('No matching subnets detected.');

      document.getElementById('inventory-shared').innerHTML = data.shared_networks.length
        ? data.shared_networks.map(g => item(g.cidr, g.agents.join(', '), [{ text: 'multi-hop candidate', tone: 'ok' }], `network:${g.cidr}`)).join('')
        : emptyText('No shared subnets detected.');

      document.getElementById('inventory-conflicts').innerHTML = data.conflicts.length
        ? data.conflicts.map(c => item(c.cidr, c.agents.join(', '), [{ text: 'overlap', tone: 'bad' }], `network:${c.cidr}`)).join('')
        : emptyText('No route conflicts detected.');

      // 3. Planner Tab
      const plans = data.chain_plans || [];
      document.getElementById('planner-chains').innerHTML = plans.length
        ? plans.map((plan, idx) => {
            const isHighlighted = activeHighlightPath && activeHighlightPath[activeHighlightPath.length - 1] === `network:${plan.target}`;
            return `
              <div class="list-item ${isHighlighted ? 'selected' : ''}" onclick="togglePlanHighlight(${idx}); event.stopPropagation();" style="cursor:pointer;">
                <div class="item-header">
                  <span>${escapeHtml(plan.target)}</span>
                  <span class="pill-badge ${plan.ready ? 'success' : 'warning'}">${plan.ready ? 'Ready' : 'Planned'}</span>
                </div>
                <div class="item-desc">${escapeHtml(chainSummary(plan))}</div>
                <div class="item-pills">
                  <span class="pill-badge">${plan.actions.length} step${plan.actions.length === 1 ? '' : 's'}</span>
                  ${plan.target_ip ? `<span class="pill-badge">${escapeHtml(plan.target_ip)}</span>` : ''}
                </div>
                ${isHighlighted ? renderPlanActionsDetail(plan) : ''}
              </div>
            `;
          }).join('')
        : emptyText('No smart access suggestions.');
    }

    async function refresh() {
      if (pollPaused) return;
      try {
        const response = await fetch('/api/network-map', { cache: 'no-store' });
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        const data = await response.json();

        document.getElementById('status-dot').classList.add('online');
        document.getElementById('status-text').textContent = 'Connected';
        document.getElementById('updated').textContent = `Updated ${new Date(data.generated_at_unix * 1000).toLocaleTimeString()}`;

        current = data;
        empty.style.display = data.nodes.length <= 1 ? 'flex' : 'none';

        // Update model data & preserve physics coordinates
        initPositions(data.nodes, data.edges);
        syncSvgDom();
        renderSide(data);

        startSimulation();
      } catch (err) {
        document.getElementById('status-dot').classList.remove('online');
        document.getElementById('status-text').textContent = 'Disconnected';
      }
    }

    // Drag & Drop
    svg.addEventListener('pointerdown', (event) => {
      const nodeEl = event.target.closest('.node');
      if (nodeEl) {
        const id = nodeEl.getAttribute('data-node-id');
        draggedNode = nodeMap.get(id);
        if (draggedNode) {
          draggedNode.pinned = true;
          nodeEl.classList.add('pinned');
          svg.setPointerCapture(event.pointerId);

          const rect = svg.getBoundingClientRect();
          const pointerX = event.clientX - rect.left;
          const pointerY = event.clientY - rect.top;
          draggedNode.x = (pointerX - viewport.x) / viewport.scale;
          draggedNode.y = (pointerY - viewport.y) / viewport.scale;
          startSimulation();
        }
      } else {
        pan.active = true;
        pan.moved = false;
        pan.x = event.clientX;
        pan.y = event.clientY;
        svg.setPointerCapture(event.pointerId);
      }
    });

    svg.addEventListener('pointermove', (event) => {
      if (draggedNode) {
        const rect = svg.getBoundingClientRect();
        const pointerX = event.clientX - rect.left;
        const pointerY = event.clientY - rect.top;
        draggedNode.x = (pointerX - viewport.x) / viewport.scale;
        draggedNode.y = (pointerY - viewport.y) / viewport.scale;
        draggedNode.vx = 0;
        draggedNode.vy = 0;
        startSimulation();
      } else if (pan.active) {
        const dx = event.clientX - pan.x;
        const dy = event.clientY - pan.y;
        if (Math.abs(dx) + Math.abs(dy) > 2) pan.moved = true;
        viewport.x += dx;
        viewport.y += dy;
        pan.x = event.clientX;
        pan.y = event.clientY;
        applyViewport();
      }
    });

    svg.addEventListener('pointerup', (event) => {
      draggedNode = null;
      pan.active = false;
      try { svg.releasePointerCapture(event.pointerId); } catch (_) {}
    });

    svg.addEventListener('click', (event) => {
      if (pan.moved) {
        pan.moved = false;
        return;
      }
      if (event.target === svg) {
        selectNode('');
      }
    });

    // Zoom controls
    document.getElementById('zoom-in').addEventListener('click', () => {
      const rect = svg.getBoundingClientRect();
      zoomAt(rect.left + rect.width / 2, rect.top + rect.height / 2, 1.15);
    });
    document.getElementById('zoom-out').addEventListener('click', () => {
      const rect = svg.getBoundingClientRect();
      zoomAt(rect.left + rect.width / 2, rect.top + rect.height / 2, 1 / 1.15);
    });
    document.getElementById('zoom-fit').addEventListener('click', fitMap);
    document.getElementById('reset-pins').addEventListener('click', resetPins);

    svg.addEventListener('wheel', (event) => {
      event.preventDefault();
      zoomAt(event.clientX, event.clientY, event.deltaY < 0 ? 1.1 : 1 / 1.1);
    }, { passive: false });

    // Pause button
    document.getElementById('pause-poll').addEventListener('click', () => {
      pollPaused = !pollPaused;
      const btn = document.getElementById('pause-poll');
      if (pollPaused) {
        btn.textContent = '▶';
        btn.title = 'Resume polling';
        btn.style.color = 'var(--warn)';
      } else {
        btn.textContent = '⏸';
        btn.title = 'Pause polling';
        btn.style.color = '';
        refresh();
      }
    });

    window.addEventListener('resize', () => {
      updateDimensions();
      startSimulation();
    });

    // Initial setup
    updateDimensions();
    setupDefs();
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
    chain_plans: Vec<ChainPlan>,
    routes: Vec<DashboardRoute>,
    shared_networks: Vec<DashboardSharedNetwork>,
    conflicts: Vec<DashboardRouteConflict>,
    port_forwards: Vec<DashboardPortal>,
    ariadne: Vec<DashboardAriadne>,
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
struct DashboardPortal {
    local_port: u16,
    agent_id: String,
    agent_name: String,
    target_host: String,
    target_port: u16,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct DashboardAriadne {
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
        let port_forwards = server.portal_snapshots().await;
        let ariadne = server.ariadne_snapshots().await;
        let agents = server.agents().read().await;
        let dwellers = server.dweller_registry().read().await;
        let mut snapshot = Self::build_snapshot(&agents, &dwellers, &port_forwards, &ariadne).await;
        drop(dwellers);
        drop(agents);
        snapshot.chain_plans = ChainManager::suggestions(server).await;
        snapshot
    }

    async fn build_snapshot(
        agents: &HashMap<String, ConnectedAgent>,
        dwellers: &DwellerRegistry,
        port_forwards: &[PortalSnapshot],
        ariadne: &[AriadneSnapshot],
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
                label: agent.transport_label.clone(),
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
                active_tunnels: ariadne.len()
                    + agents
                        .values()
                        .filter(|agent| agent.tunnel_active && !is_portal_transport(agent))
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
                .map(|forward| DashboardPortal {
                    local_port: forward.local_port,
                    agent_id: forward.agent_id.clone(),
                    agent_name: agent_name(agents, &forward.agent_id),
                    target_host: forward.target_host.clone(),
                    target_port: forward.target_port,
                })
                .collect(),
            ariadne: ariadne
                .iter()
                .map(|snapshot| DashboardAriadne {
                    agent_id: snapshot.agent_id.clone(),
                    agent_name: agent_name(agents, &snapshot.agent_id),
                    proxy_port: snapshot.proxy_port,
                })
                .collect(),
            chain_plans: Vec::new(),
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
    port_forwards: &[PortalSnapshot],
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
            kind: "portal".to_string(),
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
            detail: format!(
                "{} {} / callback: {} / hibernation: {} / tasks: {} / path: {}",
                record.socket_addr(),
                record.os,
                callback_summary(&record.callback_servers),
                hibernation_summary(&record.hibernation),
                task_summary(&record.tasks),
                path_summary(&record.path)
            ),
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
            "{} / {} / {} / last seen {}s ago",
            agent.info.hostname,
            agent.info.os,
            connectivity_summary(agent),
            elapsed
        ),
    }
}

fn connectivity_summary(agent: &ConnectedAgent) -> &'static str {
    match agent.info.connectivity.internet_access {
        InternetAccess::Confirmed => "internet confirmed",
        InternetAccess::ServerReachable => "server reachable",
        InternetAccess::RouteOnly => "route only",
        InternetAccess::Unreachable => "no outbound route",
        InternetAccess::Unknown => "internet unknown",
    }
}

fn callback_summary(callbacks: &[crate::protocol::DwellerServerEndpoint]) -> String {
    if callbacks.is_empty() {
        return "not configured".to_string();
    }
    callbacks
        .iter()
        .map(|endpoint| format!("{} ({})", endpoint.address, endpoint.transport))
        .collect::<Vec<_>>()
        .join(", ")
}

fn hibernation_summary(config: &crate::protocol::DwellerHibernationConfig) -> String {
    if config.enabled {
        format!(
            "sleep {}s jitter {}% batch {}",
            config.sleep_seconds, config.jitter_percent, config.task_batch_size
        )
    } else {
        "persistent".to_string()
    }
}

fn task_summary(tasks: &[crate::protocol::DwellerTask]) -> String {
    let pending = tasks
        .iter()
        .filter(|task| matches!(task.status, crate::protocol::DwellerTaskStatus::Pending))
        .count();
    let running = tasks
        .iter()
        .filter(|task| matches!(task.status, crate::protocol::DwellerTaskStatus::Running))
        .count();
    let failed = tasks
        .iter()
        .filter(|task| matches!(task.status, crate::protocol::DwellerTaskStatus::Failed))
        .count();
    format!(
        "{} total, {} pending, {} running, {} failed",
        tasks.len(),
        pending,
        running,
        failed
    )
}

fn path_summary(path: &[crate::protocol::DwellerPathHop]) -> String {
    if path.is_empty() {
        return "unknown".to_string();
    }
    path.iter()
        .map(|hop| {
            hop.cidr
                .as_ref()
                .map(|cidr| format!("{} via {}", hop.agent_name, cidr))
                .unwrap_or_else(|| hop.agent_name.clone())
        })
        .collect::<Vec<_>>()
        .join(" -> ")
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
    if is_portal_transport(agent) {
        format!("portal/{}", agent.transport_label)
    } else {
        format!("tun/{}", agent.transport_label)
    }
}

fn is_portal_transport(agent: &ConnectedAgent) -> bool {
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
                connectivity: Default::default(),
            },
            sender,
            transport_label: "tcp/tls".to_string(),
            quic_connection: None,
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
            &[PortalSnapshot {
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
            .any(|edge| edge.label == "tcp/tls" && edge.encrypted));
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
