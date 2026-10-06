/** Stack trace with frames visually separated from the exception header. */
export function StackTrace({ text }: { text: string }) {
  const lines = text.split(/\r?\n/);
  return (
    <pre className="stacktrace" data-testid="stacktrace">
      {lines.map((line, i) => {
        const frame = /^\s*(at |File "|#\d+ |\s{2,}\S)/.test(line);
        return (
          <div key={i} className={frame ? 'st-frame' : 'st-head'}>
            {line || ' '}
          </div>
        );
      })}
    </pre>
  );
}
