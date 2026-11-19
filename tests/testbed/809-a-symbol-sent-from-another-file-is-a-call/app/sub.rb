class Sub < Base
  def go = send(:hook_a)
end
Sub.new.go
