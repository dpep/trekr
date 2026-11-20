class Router
  def go(kind, names)
    m = "handle_#{kind}"
    names.each { |m| Panel.new.public_send(m) }
  end

  def run(params, kind)
    n = params[:action]
    n = "handle_#{kind}" if kind
    Socket.new.public_send(n)
  end
end
Router.new.go(:a, [])
Router.new.run({}, :b)
