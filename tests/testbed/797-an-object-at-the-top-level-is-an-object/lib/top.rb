io = Object.new
class << io
  def top_write; helper_top; end
end
def helper_top; 1; end
def io.other; 2; end
io.top_write
