require "erb"

class Trailing
  def render(field) = ERB.new(DATA.read).result(binding)
  def small_attrs = 1
end

__END__
<%= public_send(field) %>
