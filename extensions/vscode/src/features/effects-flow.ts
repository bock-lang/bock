// Pure effect-flow rendering helpers for the effect-flow webview (F1.5.6).
//
// `effects.ts` imports `vscode-languageclient/node`, whose package `exports`
// subpath the headless Mocha + ts-node resolver can't follow — so anything
// living there is untestable under the unit suite. This module holds the
// pure, vscode-runtime-free helpers extracted from `effects.ts`: Mermaid
// graph construction, node-id derivation, the navigation map, and the plain
// HTML builders that take already-resolved data. It imports only the
// `EffectFlow`/related types from `./effect-analyzer`, the `Target` type, and
// `escapeHtml` from `../shared/webview`. It MUST NOT import
// `vscode-languageclient`. `effects.ts` imports these back (and re-exports any
// that external callers reference).
//
// Mirrors the `annotations-scan.ts` extraction pattern.

import type {
  EffectFlow,
  HandlerBinding,
  HandlerLayer,
  Location,
} from './effect-analyzer';
import { escapeHtml } from '../shared/webview';
import type { Target } from '../shared/types';

// ─── HTML body builders ─────────────────────────────────────────────────────

export function renderEmptyState(): string {
  return `
    <h1>Effect Flow</h1>
    <p class="bock-missing">
      Place the cursor inside a function definition in an <code>.bock</code>
      file, then run <code>Bock: Show Effect Flow for Function</code>.
    </p>
    <p class="bock-missing">
      Functions without a <code>with</code> clause are pure — nothing to visualise.
    </p>`;
}

export function renderFlowBody(flow: EffectFlow, targets: Target[]): string {
  const header = `
    <h1>Effect Flow — <code>${escapeHtml(flow.functionName)}</code></h1>
    <p>
      Declared effects:
      ${
        flow.effects.length > 0
          ? flow.effects
              .map(
                (e) =>
                  `<span class="bock-badge bock-badge-effect bock-nav" data-nav-id="${escapeHtml(
                    nodeId('eff', e),
                  )}">${escapeHtml(e)}</span>`,
              )
              .join(' ')
          : '<span class="bock-missing">none — this function is pure.</span>'
      }
    </p>`;

  const diagram =
    flow.effects.length === 0
      ? '<p class="bock-missing">Pure function — no effect graph.</p>'
      : `<div class="bock-diagram" id="bock-mermaid">Rendering diagram…</div>`;

  const handlersSection = renderHandlers(flow.handlers);

  const targetButton = `
    <div class="bock-targets-row">
      <button id="bock-show-targets" type="button" class="bock-button">Show in targets</button>
    </div>
    <div id="bock-targets-panel" hidden>
      ${renderTargetsTable(flow, targets)}
    </div>`;

  return `${header}${diagram}${handlersSection}${targetButton}`;
}

export function renderHandlers(handlers: HandlerBinding[]): string {
  if (handlers.length === 0) {
    return `
      <h2>Handler resolution</h2>
      <p class="bock-missing">
        No handler found in local / module / project layers. The call site
        must provide one via <code>handling (…)</code> before execution.
      </p>`;
  }
  const byLayer: Record<HandlerLayer, HandlerBinding[]> = {
    local: [],
    module: [],
    project: [],
  };
  for (const h of handlers) byLayer[h.layer].push(h);
  const layerSection = (label: string, layer: HandlerLayer): string => {
    const rows = byLayer[layer];
    if (rows.length === 0) return '';
    const items = rows
      .map((h) => {
        const id = nodeId('hnd', `${h.effect}_${h.handler}`);
        const loc = h.location
          ? `<span class="bock-loc">${escapeHtml(locationLabel(h.location))}</span>`
          : '';
        return `<li>
          <code>${escapeHtml(h.effect)}</code>
          <span class="bock-arrow">→</span>
          <a href="#" class="bock-nav" data-nav-id="${escapeHtml(id)}"><code>${escapeHtml(h.handler)}</code></a>
          ${loc}
        </li>`;
      })
      .join('\n');
    return `<h3>${escapeHtml(label)}</h3><ul class="bock-handlers">${items}</ul>`;
  };
  return `
    <h2>Handler resolution</h2>
    ${layerSection('Local (handling blocks)', 'local')}
    ${layerSection('Module (handle declarations)', 'module')}
    ${layerSection('Project (bock.project [effects])', 'project')}`;
}

export function renderTargetsTable(flow: EffectFlow, targets: Target[]): string {
  if (flow.effects.length === 0) {
    return `<p class="bock-missing">Pure function — no target strategies needed.</p>`;
  }
  const rows = targets
    .map((t) => {
      const strategy = targetStrategy(t.id);
      return `<tr>
        <td><code>${escapeHtml(t.id)}</code></td>
        <td>${escapeHtml(t.display_name)}</td>
        <td>${escapeHtml(strategy.support)}</td>
        <td>${escapeHtml(strategy.strategy)}</td>
      </tr>`;
    })
    .join('\n');
  return `
    <h2>Target support</h2>
    <p>
      Bock's universal codegen strategy for effects is parameter passing;
      see <a href="#" class="bock-spec-link-inline" data-spec-ref="§13">§13 Transpilation</a>.
    </p>
    <table class="bock-targets">
      <thead>
        <tr><th>Target</th><th>Name</th><th>Support</th><th>Strategy</th></tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>`;
}

export function targetStrategy(id: string): {
  support: string;
  strategy: string;
} {
  switch (id) {
    case 'js':
    case 'ts':
    case 'python':
      return { support: 'Emulated', strategy: 'Parameter passing' };
    case 'rust':
      return { support: 'Emulated', strategy: 'Trait parameter' };
    case 'go':
      return { support: 'Emulated', strategy: 'Interface parameter' };
    default:
      return { support: 'Emulated', strategy: 'Parameter passing' };
  }
}

// ─── Mermaid construction ───────────────────────────────────────────────────

export function buildMermaid(flow: EffectFlow): string {
  const lines: string[] = ['graph LR'];
  const fnNode = nodeId('fn', flow.functionName);
  const fnLabel = mermaidLabel(`${flow.functionName}(…)`);
  lines.push(`  ${fnNode}[${fnLabel}]:::fnNode`);

  // Effect nodes
  for (const eff of flow.effects) {
    const id = nodeId('eff', eff);
    lines.push(`  ${id}([${mermaidLabel(eff)}]):::effNode`);
  }

  // Operation nodes (only operations we found called in the body).
  const seenOps = new Set<string>();
  for (const call of flow.callees) {
    const key = call.operation;
    if (seenOps.has(key)) continue;
    seenOps.add(key);
    const opId = nodeId('op', key);
    lines.push(`  ${opId}[["${escapeMermaid(call.operation)}()"]]:::opNode`);
  }

  // Fn → Op edges labelled with the with clause.
  const withLabel = flow.effects.join(', ');
  for (const op of seenOps) {
    const opId = nodeId('op', op);
    const label = withLabel ? `with ${escapeMermaid(withLabel)}` : 'calls';
    lines.push(`  ${fnNode} -->|${label}| ${opId}`);
  }

  // Op → Effect dashed edges (membership).
  for (const op of seenOps) {
    const call = flow.callees.find((c) => c.operation === op);
    const effName = call?.effect;
    if (!effName) continue;
    if (!flow.effects.includes(effName)) continue;
    const opId = nodeId('op', op);
    const effId = nodeId('eff', effName);
    lines.push(`  ${opId} -.->|of| ${effId}`);
  }

  // Handler nodes + Effect → Handler edges, grouped by layer.
  const handlerSeen = new Set<string>();
  for (const h of flow.handlers) {
    const handlerKey = `${h.effect}_${h.handler}`;
    const handlerId = nodeId('hnd', handlerKey);
    const effId = nodeId('eff', h.effect);
    if (!handlerSeen.has(handlerId)) {
      handlerSeen.add(handlerId);
      const layerLabel = layerTag(h.layer);
      const label = mermaidLabel(`${h.handler} [${layerLabel}]`);
      lines.push(`  ${handlerId}[${label}]:::hndNode_${h.layer}`);
    }
    lines.push(`  ${effId} -.->|handled by| ${handlerId}`);
  }

  // Styling classes.
  lines.push(
    `  classDef fnNode fill:#1f6feb,stroke:#58a6ff,color:#ffffff,stroke-width:2px;`,
    `  classDef effNode fill:#8957e5,stroke:#c4b1ff,color:#ffffff;`,
    `  classDef opNode fill:#2d333b,stroke:#768390,color:#adbac7;`,
    `  classDef hndNode_local fill:#2da44e,stroke:#56d364,color:#ffffff;`,
    `  classDef hndNode_module fill:#bf8700,stroke:#e3b341,color:#ffffff;`,
    `  classDef hndNode_project fill:#db6d28,stroke:#f0883e,color:#ffffff;`,
  );

  // Click bindings — route every interactive node through bockNavigate().
  const clickable = new Set<string>();
  clickable.add(fnNode);
  for (const eff of flow.effects) clickable.add(nodeId('eff', eff));
  for (const op of seenOps) clickable.add(nodeId('op', op));
  for (const id of handlerSeen) clickable.add(id);
  for (const id of clickable) {
    lines.push(`  click ${id} bockNavigate`);
  }

  return lines.join('\n');
}

function mermaidLabel(s: string): string {
  return `"${escapeMermaid(s)}"`;
}

export function escapeMermaid(s: string): string {
  return s.replace(/"/g, '#quot;').replace(/\|/g, '\\|');
}

// ─── Node IDs + navigation map ──────────────────────────────────────────────

export function nodeId(kind: 'fn' | 'eff' | 'op' | 'hnd', name: string): string {
  return `${kind}_${name.replace(/[^A-Za-z0-9_]/g, '_')}`;
}

export interface NavTarget {
  uri: string;
  line: number;
  column: number;
}

export function buildNavigationMap(flow: EffectFlow): Record<string, NavTarget> {
  const map: Record<string, NavTarget> = {};
  const fnId = nodeId('fn', flow.functionName);
  map[fnId] = {
    uri: flow.documentUri.toString(),
    line: flow.functionRange.start.line,
    column: flow.functionRange.start.character,
  };

  for (const eff of flow.effects) {
    const id = nodeId('eff', eff);
    const def = flow.effectDefs.find((d) => d.name === eff);
    if (def?.defined) {
      map[id] = locationToNav(def.defined);
    }
  }

  for (const call of flow.callees) {
    const id = nodeId('op', call.operation);
    if (!map[id]) map[id] = locationToNav(call.location);
  }

  for (const h of flow.handlers) {
    const id = nodeId('hnd', `${h.effect}_${h.handler}`);
    if (h.location && !map[id]) map[id] = locationToNav(h.location);
  }
  return map;
}

function locationToNav(loc: Location): NavTarget {
  return { uri: loc.uri.toString(), line: loc.line, column: loc.column };
}

// ─── Presentation helpers ───────────────────────────────────────────────────

export function layerTag(layer: HandlerLayer): string {
  switch (layer) {
    case 'local':
      return 'local';
    case 'module':
      return 'module';
    case 'project':
      return 'project';
  }
}

function locationLabel(loc: Location): string {
  const base = loc.uri.fsPath.split(/[\\/]/).pop() ?? loc.uri.fsPath;
  return `${base}:${loc.line + 1}`;
}
