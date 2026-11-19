class Labeled
  def named = 1
  def caller = render(named: named)
  def build(o) = o.send("named")
  def render(**) = nil
end
Labeled.new.caller
