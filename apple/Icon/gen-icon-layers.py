# 生成 App 图标的三层 SVG（写进 apple/Piesi/AppIcon.icon/Assets/）：白卡片（顶上拉开一个 V 形口）、拉链齿、拉头与拉片。
# 画布 1024×1024，和 Icon Composer 的 macOS 画布一致；底色渐变和玻璃效果在 AppIcon.icon/icon.json 里配。
# 用法：python3 apple/Icon/gen-icon-layers.py
import math, os
os.chdir(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "Piesi", "AppIcon.icon", "Assets"))
W=1024
cx=512; top=170; bottom=870; left=212; right=812; r=110
vy=480; vw=160
card=(f'M{left+r} {top} H{cx-vw} L{cx} {vy} L{cx+vw} {top} H{right-r} '
      f'A{r} {r} 0 0 1 {right} {top+r} V{bottom-r} A{r} {r} 0 0 1 {right-r} {bottom} '
      f'H{left+r} A{r} {r} 0 0 1 {left} {bottom-r} V{top+r} A{r} {r} 0 0 1 {left+r} {top} Z')
open('pouch.svg','w').write(f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{W}" viewBox="0 0 {W} {W}"><path fill="#FFFFFF" d="{card}"/></svg>')
TOOTH="#2C2C2E"
teeth=[]
tw,th,step=60,24,36
y=vy+56; i=0
while y+th<bottom-40:
    x = cx-tw+6 if i%2==0 else cx-6
    teeth.append(f'<rect x="{x}" y="{y}" width="{tw}" height="{th}" rx="9"/>')
    y+=step; i+=1
a=math.atan2(vw, vy-top)
for side in (-1,1):
    d=70
    while True:
        px=cx+side*math.sin(a)*d; py=vy-math.cos(a)*d
        if py<top+20: break
        # 齿垂直于口边、长在卡片一侧，并稍微伸进口里一点
        teeth.append(f'<g transform="translate({px:.1f} {py:.1f}) rotate({math.degrees(a)*side:.2f})">'
                     f'<rect x="{-12 if side>0 else -tw+12}" y="{-th/2}" width="{tw}" height="{th}" rx="9"/></g>')
        d+=step
open('zipper.svg','w').write(f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{W}" viewBox="0 0 {W} {W}"><g fill="{TOOTH}">{"".join(teeth)}</g></svg>')
sx,sy=cx,vy
slider=f'M{sx-56} {sy-30} H{sx+56} A22 22 0 0 1 {sx+78} {sy-8} V{sy+46} A22 22 0 0 1 {sx+56} {sy+68} H{sx-56} A22 22 0 0 1 {sx-78} {sy+46} V{sy-8} A22 22 0 0 1 {sx-56} {sy-30} Z'
tx0,ty0,w2,tlen=sx,sy+56,68,190
tab=(f'M{tx0-w2/2+20} {ty0} H{tx0+w2/2-20} A20 20 0 0 1 {tx0+w2/2} {ty0+20} V{ty0+tlen-w2/2} '
     f'A{w2/2} {w2/2} 0 0 1 {tx0-w2/2} {ty0+tlen-w2/2} V{ty0+20} A20 20 0 0 1 {tx0-w2/2+20} {ty0} Z')
hole=f'<circle cx="{tx0}" cy="{ty0+tlen-w2/2-6}" r="15" fill="#FFFFFF"/>'
open('pull.svg','w').write(f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{W}" viewBox="0 0 {W} {W}"><path fill="#1C1C1E" d="{tab}"/>{hole}<path fill="#3A3A3C" d="{slider}"/></svg>')
