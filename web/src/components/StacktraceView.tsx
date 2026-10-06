import { useMemo, useState } from 'react';

type Frame = {
  method: string;
  file?: string;
  line?: string;
};

type TraceBlock = {
  header?: string;
  frames: Frame[];
  notes: string[];
};

const AT_IN_RE =
  /^\s*(?:---\s*)?at\s+(.+?)(?:\s+in\s+(.+?):line\s+(\d+))?\s*$/i;
const AT_ONLY_RE = /^\s*at\s+(.+)\s*$/i;
const IN_ONLY_RE = /^\s*in\s+(.+?)(?::line\s+(\d+))?\s*$/i;
const HEADER_RE =
  /^(?:--->\s*)?([\w.+=<>`\[\],+]+(?:Exception|Error|Throwable)?)\s*:\s*(.*)$/;

function normalizeRaw(raw: string): string[] {
  let text = raw.replace(/\r\n/g, '\n');

  // If text contains inline separators without newlines, split them
  // Inline --->
  text = text.replace(/([^\n])\s*--->\s*/g, '$1\n---> ');
  // Inline InnerException: or Caused by:
  text = text.replace(/([^\n])\s+(InnerException(?:\[\d+\])?:)/gi, '$1\n$2');
  text = text.replace(/([^\n])\s+(Caused by:)/gi, '$1\n$2');
  // Inline Source:, TargetSite:, HResult:, Data:, StackTrace:
  text = text.replace(
    /([^\n])\s+((?:Source|TargetSite|HResult|Data|StackTrace):)/gi,
    '$1\n$2',
  );
  // Inline [key] = val
  text = text.replace(/([^\n])\s+(\[[^\]\n]+\]\s*=)/g, '$1\n$2');
  // Inline at ... (e.g. "at Foo() in Bar:line 1 at Baz() in Qux:line 2")
  text = text.replace(/([^\n])\s+(at\s+[\w.<>+`]+)/g, '$1\n$2');
  // Inline --- End of inner exception stack trace ---
  text = text.replace(
    /([^\n])\s+(---\s*End of inner exception[^\n]*)/gi,
    '$1\n$2',
  );

  return text.split('\n');
}

export function parseStacktrace(raw: string): TraceBlock[] {
  const lines = normalizeRaw(raw);
  const blocks: TraceBlock[] = [];
  const outerStack: TraceBlock[] = [];
  let current: TraceBlock = { frames: [], notes: [] };

  const pushCurrent = () => {
    if (current.header || current.frames.length || current.notes.length) {
      blocks.push(current);
      current = { frames: [], notes: [] };
    }
  };

  for (let i = 0; i < lines.length; i++) {
    const trimmed = lines[i]!.trim();
    if (!trimmed) continue;

    // Native .NET separators / inner-exception markers
    if (/^---\s*End of inner exception/i.test(trimmed)) {
      pushCurrent();
      if (outerStack.length > 0) {
        current = outerStack.pop()!;
      }
      continue;
    }
    if (
      /^--->\s*/.test(trimmed) ||
      /^InnerException/i.test(trimmed) ||
      /^Caused by:/i.test(trimmed)
    ) {
      if (current.header || current.frames.length || current.notes.length) {
        outerStack.push(current);
      }
      current = { frames: [], notes: [] };
      const headerLine = trimmed
        .replace(/^--->\s*/, '')
        .replace(/^InnerException(?:\[\d+\])?:\s*/i, '')
        .replace(/^Caused by:\s*/i, '');
      const header = headerLine.match(HEADER_RE);
      const label = /^Caused/i.test(trimmed) ? 'Caused by' : 'Inner exception';
      if (header) {
        current.notes.push(label);
        current.header = headerLine;
      } else if (headerLine) {
        current.notes.push(label);
        current.header = headerLine;
      } else {
        current.notes.push(trimmed);
      }
      continue;
    }

    if (/^StackTrace:?$/i.test(trimmed)) continue;
    if (/^(Source|TargetSite|HResult|Data):/i.test(trimmed)) {
      current.notes.push(trimmed);
      continue;
    }
    if (/^\[[^\]]+\]\s*=/.test(trimmed)) {
      current.notes.push(trimmed);
      continue;
    }

    const atIn = trimmed.match(AT_IN_RE);
    if (atIn) {
      current.frames.push({
        method: atIn[1]!.trim(),
        file: atIn[2]?.trim(),
        line: atIn[3],
      });
      continue;
    }

    const atOnly = trimmed.match(AT_ONLY_RE);
    if (atOnly) {
      const next = lines[i + 1]?.trim() ?? '';
      const inOnly = next.match(IN_ONLY_RE);
      if (inOnly) {
        current.frames.push({
          method: atOnly[1]!.trim(),
          file: inOnly[1]?.trim(),
          line: inOnly[2],
        });
        i += 1;
      } else {
        current.frames.push({ method: atOnly[1]!.trim() });
      }
      continue;
    }

    const header = trimmed.match(HEADER_RE);
    if (header && (header[1]!.includes('.') || /Exception|Error|Throwable/i.test(header[1]!))) {
      if (current.header || current.frames.length) pushCurrent();
      current.header = trimmed;
      continue;
    }

    current.notes.push(trimmed);
  }

  pushCurrent();
  while (outerStack.length > 0) {
    const prev = outerStack.pop()!;
    if (prev.header || prev.frames.length || prev.notes.length) {
      blocks.push(prev);
    }
  }

  return blocks.length > 0 ? blocks : [{ frames: [], notes: [raw] }];
}

function renderMethod(method: string) {
  const paren = method.indexOf('(');
  if (paren === -1) {
    return <span className="st-method">{method}</span>;
  }
  return (
    <>
      <span className="st-method">{method.slice(0, paren)}</span>
      <span className="st-params">{method.slice(paren)}</span>
    </>
  );
}

export default function StacktraceView({ stacktrace }: { stacktrace: string }) {
  const [copied, setCopied] = useState(false);
  const blocks = useMemo(() => parseStacktrace(stacktrace), [stacktrace]);

  async function copy() {
    try {
      await navigator.clipboard.writeText(stacktrace);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1200);
    } catch {
      /* ignore */
    }
  }

  return (
    <div className="stacktrace-view">
      <button
        type="button"
        className="stacktrace-copy"
        title={copied ? 'Copied' : 'Copy stacktrace'}
        onClick={copy}
        aria-label="Copy stacktrace"
      >
        {copied ? (
          <svg width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden>
            <path
              d="M3.5 8.5 6.5 11.5 12.5 4.5"
              stroke="currentColor"
              strokeWidth="1.6"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        ) : (
          <svg width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden>
            <rect x="5.5" y="5.5" width="7" height="8" rx="1.2" stroke="currentColor" strokeWidth="1.4" />
            <path
              d="M3.5 10.5V3.8A1.3 1.3 0 0 1 4.8 2.5h5.7"
              stroke="currentColor"
              strokeWidth="1.4"
              strokeLinecap="round"
            />
          </svg>
        )}
      </button>

      {blocks.map((block, bi) => (
        <div key={bi} className="stacktrace-block">
          {block.notes
            .filter((n) => /^Inner|Caused/i.test(n))
            .map((n, ni) => (
              <div key={`label-${ni}`} className="st-label">
                {n}
              </div>
            ))}
          {block.header && <div className="st-header">{block.header}</div>}
          {block.notes
            .filter((n) => !/^Inner|Caused/i.test(n))
            .map((n, ni) => (
              <div key={`note-${ni}`} className="st-note">
                {n}
              </div>
            ))}
          {block.frames.map((frame, fi) => (
            <div key={fi} className="st-frame">
              <div className="st-at">
                <span className="st-kw">at</span> {renderMethod(frame.method)}
              </div>
              {frame.file && (
                <div className="st-in">
                  <span className="st-kw">in</span>{' '}
                  <span className="st-file">
                    {frame.file}
                    {frame.line != null ? `:line ${frame.line}` : ''}
                  </span>
                </div>
              )}
            </div>
          ))}
        </div>
      ))}
    </div>
  );
}

export function isStacktraceKey(key: string): boolean {
  const lower = key.toLowerCase();
  return (
    lower === 'stacktrace' ||
    lower === 'stack_trace' ||
    lower === 'exception.stacktrace' ||
    lower === 'exception.stack_trace' ||
    lower === 'error.stack' ||
    lower === 'error.stacktrace' ||
    lower === 'error.stack_trace' ||
    lower === '@x'
  );
}

export const STACKTRACE_ATTR_KEYS = new Set([
  'exception.stacktrace',
  'exception.stackTrace',
  'exception.stack_trace',
  'stacktrace',
  'stackTrace',
  'stack_trace',
  'error.stack',
  'error.stacktrace',
  'error.stack_trace',
  '@x',
]);

/** Resolve stacktrace text from event field or exception.stacktrace attribute. */
export function resolveStacktrace(event: {
  stacktrace?: string | null;
  attributes?: Record<string, unknown>;
}): string | null {
  if (event.stacktrace && event.stacktrace.trim()) return event.stacktrace;
  const attrs = event.attributes ?? {};
  for (const [k, v] of Object.entries(attrs)) {
    if (isStacktraceKey(k) && typeof v === 'string' && v.trim()) {
      return v;
    }
  }
  return null;
}
