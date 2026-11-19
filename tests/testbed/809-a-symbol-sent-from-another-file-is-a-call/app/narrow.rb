class Foo
  def b1 = 1
end

class Other
  def b1 = 1
end
Other.new.send(:b1)
