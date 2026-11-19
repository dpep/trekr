class Called
  def build = send(:foo)
  def bar = foo
  def foo = 1
end
Called.new.bar
Called.new.build
