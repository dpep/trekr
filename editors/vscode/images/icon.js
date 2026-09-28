// Regenerates icon.svg and icon.png beside this file:
//   npm i --no-save @resvg/resvg-js && node images/icon.js
const { Resvg } = require("@resvg/resvg-js");
const fs = require("fs");
const path = require("path");

// A faceted ruby, lit from the upper left.
const gem = `
    <polygon points="30,0 110,0 140,34 70,112 0,34" fill="#c81e3a"/>
    <polygon points="30,0 50,34 0,34" fill="#e8475f"/>
    <polygon points="30,0 110,0 90,34 50,34" fill="#f06a7e"/>
    <polygon points="110,0 140,34 90,34" fill="#d62f48"/>
    <polygon points="0,34 50,34 70,112" fill="#a3142c"/>
    <polygon points="50,34 90,34 70,112" fill="#c81e3a"/>
    <polygon points="90,34 140,34 70,112" fill="#8c0f24"/>`;

// The trail: cubic segments from the lower left, one loop, up to the gem's point.
const dy = 2; // lowers the crossing so both legs' dots clear each other
const segs = [
  [[24, 224], [60, 230], [100, 222 + dy], [132, 206 + dy]],
  [[132, 206 + dy], [166, 188 + dy], [216, 184 + dy], [216, 212 + dy]],
  [[216, 212 + dy], [216, 240], [172, 244], [152, 222]],
  [[152, 222], [134, 202], [128, 170], [128, 138]],
];
const bez = (p, t) => {
  const u = 1 - t;
  return [0, 1].map((i) => u * u * u * p[0][i] + 3 * u * u * t * p[1][i] + 3 * u * t * t * p[2][i] + t * t * t * p[3][i]);
};
const pts = segs.flatMap((s) => Array.from({ length: 401 }, (_, i) => bez(s, i / 400)));

// Dots at an even arc length; the phase keeps the two legs' dots apart where they cross.
const step = 17, phase = 9.5, dots = [];
let acc = step - phase, prev = pts[0];
for (const p of pts) {
  acc += Math.hypot(p[0] - prev[0], p[1] - prev[1]);
  prev = p;
  if (acc >= step) {
    acc = 0;
    dots.push(p);
  }
}
const tip = [128, 138], last = dots[dots.length - 1];
if (Math.hypot(last[0] - tip[0], last[1] - tip[1]) > step / 2) dots.push(tip);

const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256">
<defs><linearGradient id="bg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#23293a"/><stop offset="1" stop-color="#151925"/></linearGradient><clipPath id="c"><rect width="256" height="256" rx="52"/></clipPath></defs>
<rect width="256" height="256" rx="52" fill="url(#bg)"/>
<g clip-path="url(#c)" stroke="#8fa3c7" stroke-opacity="0.13" stroke-width="1.5"><path d="M0 64H256M0 128H256M0 192H256M64 0V256M128 0V256M192 0V256"/></g>
<g fill="#e8dccb">${dots.map(([x, y]) => `<circle cx="${x.toFixed(1)}" cy="${y.toFixed(1)}" r="4.2"/>`).join("")}</g>
<g transform="translate(71 30) scale(0.815)">${gem}</g>
</svg>
`;
fs.writeFileSync(path.join(__dirname, "icon.svg"), svg);
fs.writeFileSync(path.join(__dirname, "icon.png"), new Resvg(svg, { fitTo: { mode: "width", value: 256 } }).render().asPng());
