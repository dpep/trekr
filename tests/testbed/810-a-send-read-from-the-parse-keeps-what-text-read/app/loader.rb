class Loader
  def load(opts)
    opts.each do |k, v|
      k = "#{k}="
      Settings.public_send(k, v)
    end
  end
end
