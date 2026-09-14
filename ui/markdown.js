/* Minimal safe markdown → HTML for Pronto search answers. */

function escapeHtml(value) {
  const node = document.createElement('div');
  node.textContent = value == null ? '' : String(value);
  return node.innerHTML;
}

function inlineMarkdown(text) {
  let out = escapeHtml(text);
  out = out.replace(/\[(\d+)\]/g, '<sup class="cite">$1</sup>');
  out = out.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>');
  out = out.replace(/\*(.+?)\*/g, '<em>$1</em>');
  out = out.replace(/`([^`]+)`/g, '<code>$1</code>');
  return out;
}

function isTableRow(line) {
  return line.includes('|') && !line.trim().startsWith('```');
}

function parseTableRow(line) {
  return line
    .trim()
    .replace(/^\|/, '')
    .replace(/\|$/, '')
    .split('|')
    .map(cell => cell.trim());
}

function isTableDivider(line) {
  return /^\|?[\s:-]+\|[\s|:-]+$/.test(line.trim());
}

function renderMarkdownTable(lines) {
  if (!lines.length) return '';
  const rows = lines.map(parseTableRow);
  const head = rows[0];
  const bodyRows = rows.slice(1).filter((_, index) => {
    if (index === 0 && isTableDivider(lines[1] || '')) return false;
    return true;
  });
  const startBody = isTableDivider(lines[1] || '') ? 2 : 1;
  const body = rows.slice(startBody);
  const headHtml = `<tr>${head.map(cell => `<th>${inlineMarkdown(cell)}</th>`).join('')}</tr>`;
  const bodyHtml = body.map(row =>
    `<tr>${row.map(cell => `<td>${inlineMarkdown(cell)}</td>`).join('')}</tr>`
  ).join('');
  return `<div class="search-table-wrap"><table class="search-table"><thead>${headHtml}</thead><tbody>${bodyHtml}</tbody></table></div>`;
}

function renderMarkdown(markdown) {
  const lines = String(markdown || '').replace(/\r\n/g, '\n').split('\n');
  const parts = [];
  let index = 0;

  while (index < lines.length) {
    const line = lines[index];
    const trimmed = line.trim();

    if (!trimmed) {
      index += 1;
      continue;
    }

    if (trimmed.startsWith('```')) {
      const codeLines = [];
      index += 1;
      while (index < lines.length && !lines[index].trim().startsWith('```')) {
        codeLines.push(lines[index]);
        index += 1;
      }
      index += 1;
      parts.push(`<pre class="search-code"><code>${escapeHtml(codeLines.join('\n'))}</code></pre>`);
      continue;
    }

    if (isTableRow(trimmed)) {
      const tableLines = [];
      while (index < lines.length && isTableRow(lines[index].trim())) {
        tableLines.push(lines[index].trim());
        index += 1;
      }
      parts.push(renderMarkdownTable(tableLines));
      continue;
    }

    if (trimmed.startsWith('> ')) {
      const quoteLines = [];
      while (index < lines.length && lines[index].trim().startsWith('> ')) {
        quoteLines.push(lines[index].trim().slice(2));
        index += 1;
      }
      parts.push(`<p class="search-answer-lead">${inlineMarkdown(quoteLines.join(' '))}</p>`);
      continue;
    }

    if (trimmed.startsWith('### ')) {
      parts.push(`<h4 class="search-md-h4">${inlineMarkdown(trimmed.slice(4))}</h4>`);
      index += 1;
      continue;
    }

    if (trimmed.startsWith('## ')) {
      parts.push(`<h3 class="search-md-h3">${inlineMarkdown(trimmed.slice(3))}</h3>`);
      index += 1;
      continue;
    }

    if (trimmed.startsWith('# ')) {
      parts.push(`<h2 class="search-node-heading">${inlineMarkdown(trimmed.slice(2))}</h2>`);
      index += 1;
      continue;
    }

    if (/^[-*]\s+/.test(trimmed)) {
      const items = [];
      while (index < lines.length && /^[-*]\s+/.test(lines[index].trim())) {
        items.push(lines[index].trim().replace(/^[-*]\s+/, ''));
        index += 1;
      }
      parts.push(`<ul class="search-md-list">${items.map(item => `<li>${inlineMarkdown(item)}</li>`).join('')}</ul>`);
      continue;
    }

    const paraLines = [];
    while (index < lines.length) {
      const current = lines[index].trim();
      if (!current || current.startsWith('```') || current.startsWith('> ')
        || current.startsWith('#') || /^[-*]\s+/.test(current) || isTableRow(current)) {
        break;
      }
      paraLines.push(current);
      index += 1;
    }
    parts.push(`<p class="search-node-text">${inlineMarkdown(paraLines.join(' '))}</p>`);
  }

  return parts.join('');
}

function splitMarkdownLead(markdown) {
  const lines = String(markdown || '').replace(/\r\n/g, '\n').split('\n');
  let index = 0;
  while (index < lines.length && !lines[index].trim()) index += 1;
  if (index >= lines.length) return { lead: '', rest: '' };

  if (lines[index].trim().startsWith('> ')) {
    const leadLines = [];
    while (index < lines.length && lines[index].trim().startsWith('> ')) {
      leadLines.push(lines[index].trim().slice(2));
      index += 1;
    }
    while (index < lines.length && !lines[index].trim()) index += 1;
    const lead = `> ${leadLines.join('\n> ')}`;
    return { lead, rest: lines.slice(index).join('\n') };
  }

  return { lead: '', rest: markdown };
}

window.renderMarkdown = renderMarkdown;
window.splitMarkdownLead = splitMarkdownLead;
