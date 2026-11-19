class Caller
  def run(kind) = Target.new.public_send("#{kind}_attrs")
  def opts(kind) = Target.new.__send__(:"#{kind}_opts")
  def ask(kind) = Target.new.public_send("#{kind}?")
end
Caller.new.run(:x)
