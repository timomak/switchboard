"""Compose two half logos; trace monochrome outlines into a genuine SVG."""
from pathlib import Path
from PIL import Image, ImageDraw
import json
root=Path(__file__).resolve().parent
sources=root.parent/'provider-icons'
images={k:Image.open(sources/f'{v}.png').convert('RGB') for k,v in [('claude','claude'),('openai','codex')]}
for name,img in images.items(): img.save(root/f'{name}.jpg',quality=98,subsampling=0)
# Same centered full-logo bounds for both sides; clip at the vertical midpoint.
canvas=Image.new('RGBA',(1024,1024))
for name,box in [('claude',(0,0,512,1024)),('openai',(512,0,1024,1024))]:
    full=Image.open(root/f'{name}.jpg').resize((960,960),Image.Resampling.LANCZOS)
    layer=Image.new('RGBA',(1024,1024),(255,255,255,255));layer.paste(full,(32,32))
    canvas.paste(layer.crop(box),box)
mask=Image.new('L',(1024,1024));ImageDraw.Draw(mask).rounded_rectangle((32,32,991,991),radius=210,fill=255)
canvas.putalpha(mask);canvas.save(root/'Switchboard.png')
# Boundary tracing at source resolution. SVG paths contain coordinates only.
all_paths=[]
for name,img in images.items():
    img=img.resize((128,128)); pixels=img.load(); selected=set()
    for y in range(128):
        for x in range(128):
            r,g,b=pixels[x,y]
            on=(r>225 and g>190 and b>175) if name=='claude' else max(r,g,b)<130
            if on and ((x<64) if name=='claude' else (x>=64)): selected.add((x,y))
    edges={}
    for x,y in selected:
        for neighbor,a,b in [((x,y-1),(x,y),(x+1,y)),((x+1,y),(x+1,y),(x+1,y+1)),((x,y+1),(x+1,y+1),(x,y+1)),((x-1,y),(x,y+1),(x,y))]:
            if neighbor not in selected:edges.setdefault(a,[]).append(b)
    while edges:
        start=next(iter(edges));point=start;path=[]
        while True:
            path.append(point);nxt=edges[point].pop()
            if not edges[point]:del edges[point]
            point=nxt
            if point==start:break
        # Remove redundant collinear vertices.
        simple=[]
        for i,p in enumerate(path):
            a=path[i-1];b=path[(i+1)%len(path)]
            if (p[0]-a[0])*(b[1]-p[1])!=(p[1]-a[1])*(b[0]-p[0]):simple.append(p)
        if len(simple)>2:all_paths.append(simple)
(root/'outlines.json').write_text(json.dumps(all_paths))
d=' '.join('M'+' L'.join(f'{x},{y}' for x,y in points)+' Z' for points in all_paths)
(root/'Switchboard-menubar.svg').write_text(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128"><title>Switchboard: Claude left half, OpenAI right half</title><path fill="currentColor" fill-rule="evenodd" d="{d}"/></svg>\n')
