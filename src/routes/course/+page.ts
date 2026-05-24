// URL of a course window is opaque ("/course/") — folder is resolved at runtime
// from the Tauri window label via `get_window_course_folder`. So there's
// nothing meaningful to prerender beyond a static shell. Using
// `trailingSlash = 'always'` makes adapter-static write `course/index.html`,
// which Tauri's asset protocol resolves from the URL "course/".
export const prerender = true;
export const ssr = false;
export const trailingSlash = 'always';
