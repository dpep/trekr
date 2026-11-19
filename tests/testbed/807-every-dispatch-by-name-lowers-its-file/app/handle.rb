class Handle
  def build = send(:method, :handled_only)
  def handled_only = 1
end
Handle.new.build
