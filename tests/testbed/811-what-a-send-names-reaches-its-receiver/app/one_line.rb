class OneLine
  def go(list) = send(:b2) || list.each(&method(:b2))
  def b2(x = nil) = x
end
OneLine.new.go([])
