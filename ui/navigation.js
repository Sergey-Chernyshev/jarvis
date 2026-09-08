/* Navigation stores page state, never DOM nodes or backend handles. */
(() => {
  const copy = value => JSON.parse(JSON.stringify(value));
  const key = route => `${route.view}:${route.sessionId || ''}`;
  window.createJarvisNavigation = (initial = { view: 'home' }) => {
    let current = copy(initial);
    const trail = [];
    return {
      go(next, from = current) {
        if (next.view === 'home') trail.length = 0;
        else if (key(next) !== key(from)) {
          trail.push(copy(from));
          if (trail.length > 32) trail.shift();
        }
        current = copy(next);
        return copy(current);
      },
      replace(route) { current = copy(route); },
      back() { current = trail.pop() || { view: 'home' }; return copy(current); },
      current() { return copy(current); },
      depth() { return trail.length; },
    };
  };
})();
