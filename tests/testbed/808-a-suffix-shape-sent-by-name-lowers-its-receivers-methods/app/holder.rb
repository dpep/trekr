class Holder
  def run(kind)
    name = :"#{kind}_rows"
    Target.new.public_send(name)
  end
end
