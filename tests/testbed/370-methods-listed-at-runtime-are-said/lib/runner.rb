class Runner
  def attach(context)
    Tools.public_instance_methods.each do |name|
      context.attach(name.to_s, Tools.instance_method(name))
    end
  end
end
