class Aliased
  def target = 1
  alias_method :other_name, :target
  def go = send(:other_name)
end
Aliased.new.go
