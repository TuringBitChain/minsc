export const debounce = (fn, time, timer) => (...args) => {
  if (!timer) fn(...args)
  const tailCall = !!timer
  clearTimeout(timer)
  timer = setTimeout(() => {
    timer = null
    tailCall && fn(...args)
  }, time)
}

export const encode = str =>
  encodeURIComponent(str)
    .replace(/[*._-~'!()]/g, escape)

export function findErrorLines (code, errMessage) {
  const m = errMessage.match(/ at (\d+)(?::(\d+))?/)
  if (!m) return null

  let [ , start_pos, end_pos ] = m.map(Number)
  if (start_pos >= code.length) start_pos = code.length-1
  if (!end_pos) end_pos = start_pos+1
  const LoC = code.split('\n'), lines = []

  let pos = 0, line = 0, line_start=0;
  for (; pos <= start_pos; line_start=pos, pos+=LoC.shift().length+1, line++);
  const from = { line: line-1, ch: start_pos - line_start };

  for (; pos <= end_pos && LoC.length; line_start=pos, pos += LoC.shift().length+1, line++);
  const to = { line: line-1, ch: end_pos - line_start };

  return { from, to }
}

export async function loadGist(identifier) {
  const [ gist_id, file_index ] = identifier.split(':')
  const resp = await fetch(`https://api.github.com/gists/${encodeURIComponent(gist_id)}`)
      , body = await resp.json()
  if (!body.files) return Promise.reject('gist not found')

  const filenames = Object.keys(body.files)
      , file = body.files[filenames[+file_index || 0]]
  if (!file) return Promise.reject(`file #${file_index} not found`)

  let code = file.content

  // strip off ```hack syntax highlightning
  if (code.startsWith('```hack')) code = code.trim().slice(8, -4)

  return code
}