/* Overview: provider grid grouped by capability + pinned live route log. */
const { ProviderCard, RouteLogLine, Card, StatusDot, Badge } = window.FetchiraDesignSystem_6526df;

function GroupHeader({ label, count }) {
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 12 }}>
      <span style={{ fontFamily: 'var(--font-mono)', fontSize: 11, fontWeight: 600, letterSpacing: '0.12em', textTransform: 'uppercase', color: 'var(--text-lo)' }}>{label}</span>
      <span style={{ fontFamily: 'var(--font-mono)', fontSize: 11, color: 'var(--text-faint)' }}>{count}</span>
      <span style={{ flex: 1, height: 1, background: 'var(--border-faint)' }} />
    </div>
  );
}

function LiveLog() {
  const [lines, setLines] = React.useState(() => window.FX.log.map((l, i) => ({ ...l, _id: i, fresh: false })));
  const idRef = React.useRef(window.FX.log.length);
  const [paused, setPaused] = React.useState(false);

  React.useEffect(() => {
    if (paused) return;
    const token = new URLSearchParams(location.search).get('token') || '';
    let es;
    try {
      es = new EventSource('/api/events?token=' + encodeURIComponent(token));
      es.onmessage = (e) => {
        let batch;
        try { batch = JSON.parse(e.data); } catch (err) { return; }
        if (!Array.isArray(batch) || !batch.length) return;
        setLines((prev) => [...prev.slice(-40), ...batch.map((l) => ({ ...l, _id: idRef.current++, fresh: true }))]);
      };
    } catch (err) { /* no SSE when opened as a static file */ }
    return () => { if (es) es.close(); };
  }, [paused]);

  const scrollRef = React.useRef(null);
  React.useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines]);

  return (
    <Card inset pad={0} style={{ display: 'flex', flexDirection: 'column', height: '100%', minHeight: 0, overflow: 'hidden' }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', padding: '12px 14px', borderBottom: '1px solid var(--border-faint)' }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <StatusDot tone="accent" pulse size={7} />
          <span style={{ fontFamily: 'var(--font-display)', fontSize: 14, fontWeight: 600, color: 'var(--text-hi)' }}>Live route log</span>
        </div>
        <button onClick={() => setPaused(p => !p)} style={{ background: 'transparent', border: '1px solid var(--border-hairline)', color: 'var(--text-lo)', fontFamily: 'var(--font-mono)', fontSize: 11, padding: '3px 8px', borderRadius: 'var(--r-xs)', cursor: 'pointer' }}>{paused ? '▶ resume' : '❚❚ pause'}</button>
      </div>
      <div ref={scrollRef} style={{ flex: 1, overflowY: 'auto', padding: 6, display: 'flex', flexDirection: 'column', gap: 1 }}>
        {lines.length
          ? lines.map((l) => <RouteLogLine key={l._id} {...l} />)
          : <div style={{ margin: 'auto', textAlign: 'center', fontFamily: 'var(--font-mono)', fontSize: 12, color: 'var(--text-faint)', padding: 24 }}>waiting for route activity…</div>}
      </div>
    </Card>
  );
}

function OverviewTab() {
  const groups = window.FX.groups;
  return (
    <div style={{ display: 'grid', gridTemplateColumns: 'minmax(0, 1fr) 380px', gap: 20, alignItems: 'start', height: '100%' }}>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 24 }}>
        {groups.map((g) => (
          <section key={g.id}>
            <GroupHeader label={g.label} count={`${g.providers.length} ${g.providers.length === 1 ? 'provider' : 'providers'}`} />
            <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(290px, 1fr))', gap: 14 }}>
              {g.providers.map((p) => <ProviderCard {...p} key={p.name} />)}
            </div>
          </section>
        ))}
      </div>
      <div style={{ position: 'sticky', top: 84, height: 'calc(100vh - 104px)' }}>
        <LiveLog />
      </div>
    </div>
  );
}

window.OverviewTab = OverviewTab;
