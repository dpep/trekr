class Loader
  def run(k) = eval("Knob.public_send(k)")
  def page = DATA.read
end
Loader.new.run(1)
Loader.new.page
__END__
<%= Spool.public_send(name) %>
