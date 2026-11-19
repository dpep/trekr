class Other
  def go = Base.new.send(:guarded)
end
Other.new.go
